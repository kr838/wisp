'use strict'
/*
 * Floating selection toolbar — text only.
 *
 * The pill is `width: max-content`, so its bounding box is always the true intrinsic
 * width regardless of how wide the window currently is. That removes the feedback loop
 * the previous fixed-width approach suffered from (a narrow window compressed the row,
 * the compressed width was reported, and the last action stayed clipped).
 */
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const root = document.getElementById('root')
let state = { actions: [], theme: 'light', hasProvider: false, compact: false }
let payload = null
let busy = false
let sizeObserver = null

/**
 * Exact window size (CSS px) for the pill.
 *
 * The pill is width:100% of the window, so it cannot be measured directly. Instead sum the
 * row's real contents and add the pill's own horizontal padding. The result is the window
 * size verbatim — no extra slack, because any slack would show as a strip of bare backdrop
 * on one side (the pill is centred).
 *
 * scrollWidth is used as a floor for the sum so that a momentarily-too-narrow window cannot
 * make us under-report (which is what used to clip the last action).
 */
function measure(pill) {
  const s = getComputedStyle(pill)
  const px = (v) => Number.parseFloat(v) || 0
  const pad = px(s.paddingLeft) + px(s.paddingRight)
  // Intrinsic content only. scrollWidth must NOT be used as a floor: the pill is
  // width:100% of the window, so scrollWidth tracks the window and each resize would
  // report a 1px-larger value forever (the runaway +1px loop we observed).
  return { width: Math.ceil(contentSum(pill) + pad), height: 36 }
}

/// Sum of the row's intrinsic children (chips + dividers + gaps).
function contentSum(pill) {
  const s = getComputedStyle(pill)
  const gap = px2(s.columnGap)
  const kids = [...pill.children]
  let total = kids.reduce((acc, el) => acc + el.getBoundingClientRect().width, 0)
  total += gap * Math.max(0, kids.length - 1)
  return total
}
const px2 = (v) => Number.parseFloat(v) || 0

function reportSize(pill) {
  const { width, height } = measure(pill)
  const kids = [...pill.children].map(
    (c) => `${c.textContent || c.className}:${Math.round(c.getBoundingClientRect().width)}`
  )
  invoke('toolbar_report_size', {
    size: {
      width,
      height,
      // The webview's own scale. It can differ from the window's scale_factor, and the
      // window must be sized in physical pixels using THIS value or the CSS viewport ends
      // up smaller than requested (which compresses the row and clips the last chip).
      dpr: window.devicePixelRatio || 1,
      innerH: window.innerHeight,
      hasPayload: !!(payload && payload.text),
      diag: { width, inner: window.innerWidth, pill: Math.round(pill.getBoundingClientRect().width), kids, dpr: window.devicePixelRatio }
    }
  })
}

function render() {
  document.documentElement.setAttribute('data-theme', state.theme === 'dark' ? 'dark' : 'light')
  root.innerHTML = ''

  const pill = document.createElement('div')
  pill.className = 'pill' + (busy ? ' busy' : '')

  // The toolbar shows exactly the actions the user enabled — nothing is hardcoded.
  // Search is a normal action (default off) and Settings lives in the tray only.
  // Everything here is a single, uniform row: one padding, one font size, no trailing
  // utility group, so the bar reads as one consistent strip.
  const list = state.actions || []
  if (!payload || !payload.text) {
    pill.appendChild(hintEl('未选中文本'))
  } else if (list.length === 0) {
    pill.appendChild(hintEl('无可用动作，请在设置中启用'))
  } else {
    list.forEach((a) => {
      pill.appendChild(actionBtn(a.name || '处理', a.color, false, () => onClick(a)))
    })
  }

  root.appendChild(pill)

  requestAnimationFrame(() => {
    reportSize(pill)
    if (sizeObserver) sizeObserver.disconnect()
    // Re-report if the layout settles later (font metrics, emoji, etc.).
    sizeObserver = new ResizeObserver(() => reportSize(pill))
    sizeObserver.observe(pill)
  })
}

/// A text action. `color` (hex) gives the chip its own accent; `muted` renders it in the
/// neutral colour (used for the trailing search/settings actions).
function actionBtn(label, color, muted, onClick) {
  const b = document.createElement('button')
  b.type = 'button'
  b.className = 'act' + (muted ? ' muted' : '')
  if (color) b.style.setProperty('--c', color)
  b.textContent = label
  b.title = label
  b.addEventListener('click', onClick)
  return b
}

function hintEl(text) {
  const s = document.createElement('span')
  s.className = 'hint'
  s.textContent = text
  return s
}

async function onClick(a) {
  if (busy) return
  busy = true
  render()
  try {
    await invoke('run_action', { actionId: a.id })
  } finally {
    busy = false
    render()
  }
}

window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') invoke('toolbar_hide')
})

listen('toolbar-payload', (ev) => {
  payload = ev.payload
  busy = false
  render()
})
listen('settings-changed', () => {
  invoke('toolbar_get_state').then((s) => {
    state = Object.assign({}, state, s || {})
    render()
  })
})

/// Appearance (theme + material tint) is native-side; here we only apply the tint opacity.
function applyAppearance(p) {
  const el = document.documentElement
  el.setAttribute('data-theme', p.theme === 'dark' ? 'dark' : 'light')
  // Material drives both the native backdrop (set in Rust) and this CSS fallback: with
  // "none" there is no backdrop to reveal, so the surface must become opaque.
  el.setAttribute('data-material', p.material || 'mica')
  el.style.setProperty('--glass-a', String(p.glassOpacity ?? 0.68))
}

Promise.all([invoke('toolbar_get_state'), invoke('ui_prefs')]).then(([s, p]) => {
  state = Object.assign({}, state, s || {})
  applyAppearance(p || {})
  render()
})
