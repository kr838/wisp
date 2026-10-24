'use strict'
/*
 * Result window.
 *
 * Renders a conversation: the action's answer is the first turn, and 追问 appends further
 * turns. Answers arrive as Markdown and are rendered with the vendored `marked`.
 *
 * Rendering strategy: while streaming, text is appended as PLAIN text (re-parsing Markdown
 * on every chunk would be wasteful and would fight the caret), then re-rendered as Markdown
 * once the turn completes.
 *
 * Every panel is its own window (`action-1`, `action-2`, …), so this script never has to
 * coordinate with another panel's state.
 */
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const $ = (id) => document.getElementById(id)
const els = {
  name: $('name'),
  model: $('model'),
  badge: $('think-badge'),
  thinkWrap: $('think-wrap'),
  think: $('think'),
  thinkState: $('think-state'),
  srcWrap: $('src-wrap'),
  src: $('src'),
  srcHint: $('src-hint'),
  thread: $('thread'),
  empty: $('empty'),
  stop: $('btn-stop'),
  retry: $('btn-retry'),
  copy: $('btn-copy'),
  askInput: $('ask-input'),
  askBtn: $('btn-ask')
}
const THINK_LABELS = { off: '不思考', low: '轻度思考', medium: '思考', high: '深度思考' }

let ctx = null
let reasoning = ''
let streaming = false
/** Markdown container currently being streamed into. */
let currentAnswer = null
/** Raw text of the current answer / of the most recently finished one. */
let currentText = ''
let lastAnswerText = ''
/** Set when the first turn has been started, so a re-shown window does not double-run. */
let started = false

// ---- markdown ---------------------------------------------------------------

function renderMarkdown(el, text) {
  try {
    // `marked` is provided by the vendored UMD build (marked.umd.js).
    el.innerHTML = window.marked.parse(text, { breaks: true, gfm: true })
  } catch {
    // Never show a blank result: fall back to plain text if parsing fails.
    el.textContent = text
  }
  // Links must not navigate the webview; hand them to the OS browser.
  for (const a of el.querySelectorAll('a[href]')) {
    const href = a.getAttribute('href')
    a.addEventListener('click', (e) => {
      e.preventDefault()
      if (/^https?:/i.test(href)) invoke('open_external', { url: href })
    })
  }
}

// ---- ui helpers -------------------------------------------------------------

function setEmpty(text, isError) {
  if (!text) {
    els.empty.classList.add('hidden')
    els.empty.classList.remove('error')
    return
  }
  els.empty.textContent = text
  els.empty.classList.remove('hidden')
  els.empty.classList.toggle('error', !!isError)
}

function setBusy(busy) {
  streaming = busy
  // Only one of 发送 / 停止 is visible at a time, so the footer stays a single row.
  els.stop.hidden = !busy
  els.askBtn.hidden = busy
  els.askBtn.disabled = busy
  els.askInput.disabled = busy
  if (busy) els.retry.hidden = true
}

function scrollThread() {
  els.thread.scrollTop = els.thread.scrollHeight
}

/** Append a question bubble for a follow-up turn. */
function addQuestion(text) {
  const q = document.createElement('div')
  q.className = 'q'
  q.textContent = text
  els.thread.appendChild(q)
  scrollThread()
}

/** Open a new, empty answer block and return its markdown container. */
function beginAnswer() {
  const wrap = document.createElement('div')
  wrap.className = 'a'
  const md = document.createElement('div')
  md.className = 'md'
  wrap.appendChild(md)
  els.thread.appendChild(wrap)
  currentAnswer = md
  currentText = ''
  return md
}

/** Stream raw text into the current answer (plain text + caret). */
function appendChunk(text) {
  if (!currentAnswer) beginAnswer()
  currentText += text
  currentAnswer.textContent = currentText
  const caret = document.createElement('span')
  caret.className = 'caret'
  currentAnswer.appendChild(caret)
  scrollThread()
}

/** Finish the current answer: render its Markdown and drop the caret. */
function finishAnswer(finalText) {
  if (typeof finalText === 'string' && finalText && !currentText) currentText = finalText
  if (!currentAnswer) beginAnswer()
  renderMarkdown(currentAnswer, currentText)
  lastAnswerText = currentText
  scrollThread()
}

function paintReasoning() {
  if (!reasoning) {
    els.thinkWrap.hidden = true
    return
  }
  els.thinkWrap.hidden = false
  els.think.textContent = reasoning
  els.thinkState.textContent = streaming ? '思考中…' : '已完成'
}

async function applyAppearance() {
  try {
    const p = await invoke('ui_prefs')
    const el = document.documentElement
    el.setAttribute('data-theme', p.theme === 'dark' ? 'dark' : 'light')
    el.setAttribute('data-material', p.material || 'mica')
    el.style.setProperty('--glass-a', String(p.glassOpacity ?? 0.62))
  } catch {
    /* keep defaults */
  }
}

// ---- lifecycle --------------------------------------------------------------

async function begin() {
  if (started) return
  started = true
  await applyAppearance()
  ctx = await invoke('action_get_context')
  if (!ctx) {
    setEmpty('无法读取动作状态，请关闭后重试。', true)
    return
  }

  els.name.textContent = ctx.action.name || '处理'
  if (ctx.action.color) document.documentElement.style.setProperty('--c', ctx.action.color)

  // Header metadata reuses the otherwise empty space on the title line, so it costs no
  // extra height. The badge only appears when the model actually supports thinking.
  els.model.textContent = ctx.provider ? ctx.modelName || '' : ''
  const level = ctx.thinkingLevel
  els.badge.hidden = !(ctx.thinkingSupported && level && level !== 'auto')
  els.badge.textContent = els.badge.hidden ? '' : THINK_LABELS[level] || level

  // Source text: collapsed by default, with a length hint so it stays discoverable.
  const chars = (ctx.text || '').length
  els.src.textContent = ctx.text || ''
  els.srcHint.textContent = chars ? chars + ' 字' : ''
  els.srcWrap.open = false

  els.thinkWrap.hidden = true
  els.thinkWrap.open = false
  reasoning = ''
  els.thread.innerHTML = ''
  els.thread.appendChild(els.empty)
  currentAnswer = null

  if (!ctx.provider) {
    setEmpty('尚未配置 AI 模型。打开设置，在「模型」页添加一个提供商。', true)
    setBusy(false)
    els.stop.hidden = true
    els.askInput.disabled = true
    els.askBtn.disabled = true
    return
  }

  // A fresh panel starts here; 重试 re-enters through the same path with `started` cleared.
  setBusy(true)
  setEmpty('正在生成…')
  beginAnswer()
  await invoke('action_run')
}

// ---- follow-up --------------------------------------------------------------

async function sendFollowUp() {
  const text = els.askInput.value.trim()
  if (!text || streaming) return
  els.askInput.value = ''
  autoGrow()
  els.askInput.disabled = true
  els.askBtn.disabled = true
  const res = await invoke('action_ask', { text })
  if (res && res.ok === false) {
    // The question was not sent: restore it so the user does not lose their typing.
    els.askInput.disabled = false
    els.askBtn.disabled = false
    els.askInput.value = text
    autoGrow()
    setEmpty(res.error || '发送失败', true)
  }
}

function autoGrow() {
  const t = els.askInput
  t.style.height = 'auto'
  t.style.height = Math.min(t.scrollHeight, 96) + 'px'
}

// ---- events -----------------------------------------------------------------

els.stop.addEventListener('click', () => invoke('action_stop'))
els.retry.addEventListener('click', () => {
  // Replay the first turn from a clean slate. The backend resets its conversation to the
  // same initial messages, so retrying never duplicates the selected text in the history.
  started = false
  streaming = false
  currentText = ''
  lastAnswerText = ''
  reasoning = ''
  els.retry.hidden = true
  els.askInput.disabled = false
  els.askBtn.disabled = false
  els.thinkWrap.hidden = true
  els.thinkWrap.open = false
  els.thread.innerHTML = ''
  els.thread.appendChild(els.empty)
  currentAnswer = null
  begin()
})
els.copy.addEventListener('click', async () => {
  const text = lastAnswerText || currentText
  if (!text) return
  await invoke('action_copy', { text })
  const prev = els.copy.textContent
  els.copy.textContent = '已复制'
  setTimeout(() => (els.copy.textContent = prev), 1200)
})
$('btn-close').addEventListener('click', () => invoke('action_close'))

els.askBtn.addEventListener('click', sendFollowUp)
els.askInput.addEventListener('input', autoGrow)
els.askInput.addEventListener('keydown', (e) => {
  // Enter sends; Shift+Enter inserts a newline. Never fire while composing IME text.
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault()
    sendFollowUp()
  }
})
// Ctrl/Cmd+C on a text selection copies the app's own result, not the page source.
window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') invoke('action_close')
})

listen('action-start', () => begin())
listen('action-turn', (e) => {
  // A follow-up turn: show the question, then open a fresh answer block.
  const user = e && e.payload ? e.payload.user : ''
  addQuestion(user || '')
  setEmpty('')
  beginAnswer()
})
listen('action-reasoning', (e) => {
  reasoning += e.payload
  paintReasoning()
})
listen('action-chunk', (e) => {
  setEmpty('')
  appendChunk(e.payload)
})
listen('action-done', (e) => {
  setBusy(false)
  paintReasoning()
  finishAnswer(e.payload)
  setEmpty('')
})
listen('action-error', (e) => {
  setBusy(false)
  paintReasoning()
  if (currentText) {
    finishAnswer()
    setEmpty('')
  } else {
    setEmpty(e.payload || '请求失败', true)
  }
  // Retry is only meaningful once a turn has failed.
  els.retry.hidden = false
})
listen('action-aborted', () => {
  setBusy(false)
  paintReasoning()
  if (currentText) finishAnswer()
  setEmpty(currentText ? '' : '已停止。')
})

begin()
