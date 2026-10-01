'use strict'
/* Action result window (Tauri). */
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const $ = (id) => document.getElementById(id)
const els = {
  name: $('name'), model: $('model'),
  badge: $('think-badge'), thinkWrap: $('think-wrap'), think: $('think'),
  thinkState: $('think-state'), src: $('src'), out: $('out'), empty: $('empty'),
  stop: $('btn-stop'), retry: $('btn-retry'), copy: $('btn-copy'), settings: $('btn-settings')
}
const THINK_LABELS = { off: '不思考', low: '轻度思考', medium: '思考', high: '深度思考' }

let ctx = null
let full = ''
let reasoning = ''
let streaming = false

function setEmpty(text, isError) {
  if (!text) { els.empty.classList.add('hidden'); els.empty.classList.remove('error'); return }
  els.empty.textContent = text
  els.empty.classList.remove('hidden')
  els.empty.classList.toggle('error', !!isError)
}
function paint(withCaret) {
  els.out.textContent = full
  if (withCaret) {
    const c = document.createElement('span')
    c.className = 'caret'
    els.out.appendChild(c)
  }
  els.out.parentElement.scrollTop = els.out.parentElement.scrollHeight
}
function paintReasoning() {
  if (!reasoning) { els.thinkWrap.hidden = true; return }
  els.thinkWrap.hidden = false
  els.think.textContent = reasoning
  els.thinkState.textContent = streaming ? '思考中…' : '已完成'
  els.think.scrollTop = els.think.scrollHeight
}
function setBusy(b) {
  streaming = b
  els.stop.disabled = !b
  els.retry.disabled = b
}
/// Paint-time appearance. The window backdrop (mica/acrylic) is applied natively; here we
/// only set the tint opacity over it, so lowering it reveals more of the material.
async function applyAppearance() {
  try {
    const p = await invoke('ui_prefs')
    document.documentElement.setAttribute('data-theme', p.theme === 'dark' ? 'dark' : 'light')
    document.documentElement.style.setProperty('--glass-a', String(p.glassOpacity ?? 0.62))
    if (p.material === 'none') {
      // No backdrop: fall back to an opaque surface so the window is still readable.
      document.documentElement.style.setProperty('--glass-solid', '1')
    }
  } catch {
    /* keep defaults */
  }
}

async function begin() {
  await applyAppearance()
  ctx = await invoke('action_get_context')
  if (!ctx) return
  els.name.textContent = ctx.action.name || '处理'
  if (ctx.action.color) document.documentElement.style.setProperty('--c', ctx.action.color)
  els.model.textContent = ctx.provider ? ctx.modelName : '未配置模型'
  els.src.textContent = ctx.text || ''
  els.badge.textContent = ctx.thinkingSupported && ctx.thinkingLevel && ctx.thinkingLevel !== 'auto'
    ? (THINK_LABELS[ctx.thinkingLevel] || ctx.thinkingLevel) : ''
  full = ''
  reasoning = ''
  els.thinkWrap.hidden = true
  els.thinkWrap.open = false
  paintReasoning()

  if (!ctx.provider) {
    paint(false)
    setEmpty('尚未配置 AI 模型。点击右下角「设置」添加模型提供商后即可使用。', true)
    setBusy(false); els.stop.disabled = true; els.retry.disabled = true
    return
  }
  setBusy(true)
  setEmpty('正在生成…')
  await invoke('action_run')
}

els.stop.addEventListener('click', () => invoke('action_stop'))
els.retry.addEventListener('click', () => { full = ''; els.out.textContent = ''; begin() })
els.copy.addEventListener('click', async () => {
  if (!full) return
  await invoke('action_copy', { text: full })
  const prev = els.copy.textContent
  els.copy.textContent = '已复制'
  setTimeout(() => (els.copy.textContent = prev), 1200)
})
els.settings.addEventListener('click', () => invoke('open_settings'))
$('btn-close').addEventListener('click', () => invoke('action_close'))

listen('action-start', () => begin())
listen('action-chunk', (e) => { full += e.payload; setEmpty(''); paint(true) })
listen('action-reasoning', (e) => { reasoning += e.payload; paintReasoning() })
listen('action-done', (e) => {
  if (typeof e.payload === 'string' && e.payload && !full) full = e.payload
  setBusy(false); paintReasoning()
  setEmpty(full ? '' : '模型没有返回内容。', !full)
  paint(false)
})
listen('action-error', (e) => {
  setBusy(false); paintReasoning()
  if (!full) setEmpty(e.payload || '请求失败', true)
  else { setEmpty(''); paint(false) }
})
listen('action-aborted', () => {
  setBusy(false); paintReasoning()
  setEmpty(full ? '' : '已停止。')
  paint(false)
})

begin()
