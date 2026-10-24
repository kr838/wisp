'use strict'
/*
 * Floating selection toolbar — text only.
 *
 * The pill is `width: max-content`, so its bounding box is always the true intrinsic
 * width regardless of how wide the window currently is. That removes the feedback loop
 * a fixed-width approach suffers from (a narrow window compresses the row, the compressed
 * width is reported, and the last action stays clipped).
 */
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const root = document.getElementById('root')
let state = { actions: [], theme: 'light', hasProvider: false, compact: false }
let payload = null
let busy = false
let sizeObserver = null
/** Message shown instead of the action row (e.g. a missing provider). */
let notice = null

/**
 * Exact window size (CSS px) for the pill.
 *
 * The pill is width:100% of the window, so it cannot be measured directly. Instead sum the
 * row's real contents and add the pill's own horizontal padding. No slack is added, because
 * any slack would show as a strip of bare backdrop beside the pill.
 *
 * scrollWidth must NOT be used: the pill fills the window, so scrollWidth tracks the window
 * and every resize would report a 1px-larger value forever (a runaway +1px loop).
 */
function measure(pill) {
  const s = getComputedStyle(pill)
  const px = (v) => Number.parseFloat(v) || 0
  const pad = px(s.paddingLeft) + px(s.paddingRight)
  return { width: Math.ceil(contentSum(pill) + pad), height: 36 }
}

/// Sum of the row's intrinsic children (chips + gaps).
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
  invoke('toolbar_report_size', {
    size: {
      width,
      height,
      // The webview's own scale. It can differ from the window's scale_factor, and the
      // window must be sized in physical pixels using THIS value.
      dpr: window.devicePixelRatio || 1,
      hasPayload: !!(payload && payload.text)
    }
  })
}

function render() {
  document.documentElement.setAttribute('data-theme', state.theme === 'dark' ? 'dark' : 'light')
  root.innerHTML = ''

  const pill = document.createElement('div')
  pill.className = 'pill' + (busy ? ' busy' : '')

  const list = state.actions || []
  if (notice) {
    pill.appendChild(hintEl(notice.text))
    if (notice.action) pill.appendChild(actionBtn(notice.action.label, null, true, notice.action.run))
  } else if (!payload || !payload.text) {
    pill.appendChild(hintEl('未选中文本'))
  } else if (list.length === 0) {
    pill.appendChild(hintEl('无可用动作'))
  } else {
    list.forEach((a) => {
      pill.appendChild(actionBtn(a.name || '处理', a.color, false, () => onClick(a)))
    })
  }

  root.appendChild(pill)

  requestAnimationFrame(() => {
    reportSize(pill)
    if (sizeObserver) sizeObserver.disconnect()
    // Re-report if the layout settles later (font metrics).
    sizeObserver = new ResizeObserver(() => reportSize(pill))
    sizeObserver.observe(pill)
  })
}

/// A text action. `color` (hex) gives the chip its own accent; `muted` renders it neutral.
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
  notice = null
  render()
  try {
    const res = await invoke('run_action', { actionId: a.id })
    if (res && res.ok === false) {
      showError(res.error || '动作执行失败')
    }
  } catch (err) {
    showError(String((err && err.message) || err))
  } finally {
    busy = false
    render()
  }
}

/// A failed action must not look like a no-op. The bar stays visible with the reason and,
/// when the fix is configuration, a shortcut to the settings window.
function showError(message) {
  const needsSetup = /模型|提供商|未配置/.test(message)
  notice = {
    text: message,
    action: needsSetup ? { label: '打开设置', run: () => invoke('open_settings') } : null
  }
}

window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') invoke('toolbar_hide')
})

listen('toolbar-payload', (ev) => {
  payload = ev.payload
  busy = false
  notice = null
  render()
})
listen('settings-changed', () => {
  invoke('toolbar_get_state').then((s) => {
    state = Object.assign({}, state, s || {})
    render()
  })
})

/// Appearance (theme + tint) is applied natively; here only the tint opacity and theme are
/// set on the DOM.
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
