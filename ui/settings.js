'use strict'
/* Settings window (Tauri). */
;(function () {
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const content = document.getElementById('content')
const toastEl = document.getElementById('toast')
let S = null
let tab = 'general'

const PROVIDER_PRESETS = [
  // Base URLs taken from Cherry Studio's provider registry (official endpoints).
  { label: 'OpenAI', apiBase: 'https://api.openai.com/v1', model: 'gpt-4o-mini' },
  { label: 'DeepSeek', apiBase: 'https://api.deepseek.com/v1', model: 'deepseek-chat' },
  { label: 'Moonshot', apiBase: 'https://api.moonshot.cn/v1', model: 'moonshot-v1-8k' },
  { label: '智谱 GLM', apiBase: 'https://open.bigmodel.cn/api/paas/v4', model: 'glm-4-flash' },
  { label: '通义百炼', apiBase: 'https://dashscope.aliyuncs.com/compatible-mode/v1', model: 'qwen-plus' },
  { label: '硅基流动', apiBase: 'https://api.siliconflow.cn/v1', model: 'Qwen/Qwen2.5-7B-Instruct' },
  { label: 'MiniMax', apiBase: 'https://api.minimaxi.com/v1', model: 'MiniMax-Text-01' },
  { label: 'OpenRouter', apiBase: 'https://openrouter.ai/api/v1', model: 'openai/gpt-4o-mini' },
  { label: 'OpenCode Go', apiBase: 'https://opencode.ai/zen/go/v1', model: 'deepseek-v4.1-flash' },
  { label: 'OpenCode Zen', apiBase: 'https://opencode.ai/zen/v1', model: 'deepseek-v4.1-flash' },
  { label: 'Ollama', apiBase: 'http://localhost:11434/v1', model: 'llama3.1' },
  { label: 'LM Studio', apiBase: 'http://localhost:1234/v1', model: 'local-model' }
]

// Rainbow palette used to colour actions (replaces the old emoji icons).
const RAINBOW = [
  '#3b6fe0', '#7c3aed', '#c2410c', '#0f766e',
  '#be185d', '#4d7c0f', '#0e7490', '#9333ea'
]

const THINKING_LEVELS = [
  { value: 'auto', label: '自动' },
  { value: 'off', label: '关闭思考' },
  { value: 'low', label: '轻度' },
  { value: 'medium', label: '标准' },
  { value: 'high', label: '深度' }
]
const THINKING_CAPS = [
  { value: 'auto', label: '自动识别' },
  { value: 'on', label: '支持' },
  { value: 'off', label: '不支持' }
]

const genId = (p) => p + '-' + Math.random().toString(36).slice(2, 9)
function toast(m) {
  toastEl.textContent = m
  toastEl.classList.add('show')
  clearTimeout(toast._t)
  toast._t = setTimeout(() => toastEl.classList.remove('show'), 1700)
}

// ---- saving ---------------------------------------------------------------
// Edits auto-save (debounced) and confirm with a brief toast. Enter, and leaving the window,
// force an immediate write.
let saveTimer = null
let pendingPatch = null

async function flushSave() {
  clearTimeout(saveTimer)
  saveTimer = null
  if (!pendingPatch) return
  const patch = pendingPatch
  pendingPatch = null
  try {
    S = await invoke('settings_set', { patch })
    toast('已保存')
  } catch (err) {
    // Put the patch back so a retry (or Enter) can try again.
    pendingPatch = Object.assign({}, patch, pendingPatch || {})
    toast('保存失败：' + (err && err.message ? err.message : err))
  }
}

function scheduleSave(patch) {
  pendingPatch = Object.assign({}, pendingPatch || {}, patch)
  clearTimeout(saveTimer)
  saveTimer = setTimeout(flushSave, 400)
}

async function saveNow(patch) {
  pendingPatch = Object.assign({}, pendingPatch || {}, patch)
  await flushSave()
}

/// Any text field commits on Enter, so "type then press Enter" always saves.
function onEnterSave(input) {
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault()
      flushSave()
      input.blur()
    }
  })
  return input
}

function el(tag, props, children) {
  const n = document.createElement(tag)
  if (props) for (const [k, v] of Object.entries(props)) {
    if (v == null) continue
    if (k === 'class') n.className = v
    else if (k === 'text') n.textContent = v
    else if (k.startsWith('on') && typeof v === 'function') n.addEventListener(k.slice(2), v)
    else if (k === 'dataset') Object.assign(n.dataset, v)
    else n.setAttribute(k, v)
  }
  if (children != null) for (const c of (Array.isArray(children) ? children : [children])) {
    if (c == null) continue
    n.appendChild(typeof c === 'string' ? document.createTextNode(c) : c)
  }
  return n
}
function section(title, body) {
  return el('div', { class: 'section' }, [el('h2', { text: title }), body])
}
function switchRow(label, checked, onChange) {
  const sw = el('button', { class: 'switch' + (checked ? ' on' : ''), type: 'button' })
  sw.addEventListener('click', () => { const next = !sw.classList.contains('on'); sw.classList.toggle('on', next); onChange(next) })
  return el('div', { class: 'row' }, [el('div', { class: 'label', text: label }), el('div', { class: 'control' }, [sw])])
}
function selectRow(label, value, options, onChange) {
  const sel = el('select')
  for (const o of options) { const op = el('option', { value: o.value, text: o.label }); if (o.value === value) op.selected = true; sel.appendChild(op) }
  sel.addEventListener('change', () => onChange(sel.value))
  return el('div', { class: 'row' }, [el('div', { class: 'label', text: label }), el('div', { class: 'control' }, [sel])])
}
function iconBtn(text, title, disabled, onClick) {
  return el('button', { class: 'btn sm', text, title, style: disabled ? 'opacity:.4;pointer-events:none' : '', onclick: onClick })
}

// ---- general ----
function renderGeneral() {
  content.appendChild(section('通用', el('div', {}, [
    switchRow('启用 Wisp', S.enabled, (v) => saveNow({ enabled: v }).then(render)),
    selectRow('触发方式', S.triggerMode,
      [{ value: 'selected', label: '选中即弹出' }, { value: 'ctrlkey', label: '按住 Ctrl' }],
      (v) => saveNow({ triggerMode: v }).then(render)),
    switchRow('结果窗口跟随悬浮条', S.followToolbar, (v) => saveNow({ followToolbar: v })),
    switchRow('剪贴板辅助取词', S.clipboardFallback, (v) => saveNow({ clipboardFallback: v }))
  ])))
  content.appendChild(section('应用过滤', el('div', {}, [
    selectRow('过滤模式', S.filterMode,
      [{ value: 'default', label: '默认' }, { value: 'blacklist', label: '黑名单' }, { value: 'whitelist', label: '白名单' }],
      (v) => saveNow({ filterMode: v }).then(render)),
    S.filterMode !== 'default'
      ? el('div', { class: 'field' }, [
          el('label', { text: '程序名，每行一个' }),
          (() => {
            const ta = el('textarea', { placeholder: 'chrome.exe\nnotepad.exe', rows: '4' })
            ta.value = (S.filterList || []).join('\n')
            ta.addEventListener('input', () => scheduleSave({ filterList: ta.value.split('\n').map((s) => s.trim()).filter(Boolean) }))
            return ta
          })()
        ])
      : null
  ])))
  content.appendChild(section('外观', el('div', {}, [
    selectRow('悬浮条主题', S.theme, [{ value: 'light', label: '浅色' }, { value: 'dark', label: '深色' }], (v) => saveNow({ theme: v }).then(render)),
    selectRow('窗口材质', S.material || 'mica', [
      { value: 'mica', label: 'Mica' },
      { value: 'mica_alt', label: 'Mica Alt' },
      { value: 'acrylic', label: 'Acrylic' },
      { value: 'none', label: '无' }
    ], (v) => saveNow({ material: v })),
    selectRow('材质明暗', S.materialDark || 'auto', [
      { value: 'auto', label: '跟随系统' },
      { value: 'light', label: '浅色' },
      { value: 'dark', label: '深色' }
    ], (v) => saveNow({ materialDark: v })),
    el('div', { class: 'row' }, [
      el('div', { class: 'label', text: '不透明度' }),
      el('div', { class: 'control' }, [
        (() => {
          const val = el('span', { class: 'rangeval', text: String(Math.round((S.glassOpacity ?? 0.62) * 100)) + '%' })
          const r = el('input', { type: 'range', min: '15', max: '95', step: '1' })
          r.value = String(Math.round((S.glassOpacity ?? 0.62) * 100))
          r.addEventListener('input', () => { val.textContent = r.value + '%' })
          r.addEventListener('change', () => saveNow({ glassOpacity: Number(r.value) / 100 }))
          return el('span', { class: 'range-wrap' }, [r, val])
        })()
      ])
    ]),
    el('div', { class: 'field' }, [
      el('label', { text: '搜索引擎地址' }),
      (() => { const i = el('input', { type: 'text', value: S.searchUrl || '' }); i.addEventListener('input', () => scheduleSave({ searchUrl: i.value.trim() })); return i })()
    ])
  ])))
}

// ---- actions ----
function renderActions() {
  const list = el('div', { class: 'card-list' })
  ;(S.actions || []).forEach((a, index) => {
    // Resolve the CURRENT action on every edit: saving replaces `S` with fresh objects, so a
    // captured reference goes stale and the next keystroke would be discarded.
    const cur = () => (S.actions || []).find((x) => x.id === a.id) || a
    const isLocal = a.builtin === 'copy' || a.builtin === 'search'
    const card = el('div', { class: 'item-card' + (a.enabled === false ? ' is-off' : '') })
    card.appendChild(el('div', { class: 'item-head' }, [
      el('span', { class: 'swatch', style: `background:${a.color || RAINBOW[0]}` }),
      el('div', { class: 'grow' }, [
        el('div', { class: 'nm' }, [
          el('span', { text: a.name || '未命名' }),
          isLocal ? el('span', { class: 'badge', text: '本地' }) : null,
          a.enabled === false ? el('span', { class: 'badge danger', text: '已停用' }) : null
        ]),
        el('div', { class: 'sub', text: isLocal ? '不调用模型' : (a.prompt || '').replace(/\s+/g, ' ').slice(0, 60) })
      ]),
      el('div', { class: 'item-actions' }, [
        iconBtn('↑', '上移', index === 0, () => moveAction(index, -1)),
        iconBtn('↓', '下移', index === (S.actions.length - 1), () => moveAction(index, 1)),
        (() => {
          const sw = el('button', { class: 'switch' + (a.enabled !== false ? ' on' : ''), type: 'button' })
          sw.addEventListener('click', () => { const q = cur(); q.enabled = q.enabled === false; saveNow({ actions: S.actions }).then(render) })
          return sw
        })(),
        el('button', { class: 'btn sm danger', text: '删除', onclick: () => { S.actions.splice(index, 1); saveNow({ actions: S.actions }).then(render) } })
      ])
    ]))
    const body = el('div', { class: 'item-body' })
    const nameInput = el('input', { type: 'text', value: a.name || '' })
    onEnterSave(nameInput)
    nameInput.addEventListener('input', () => { cur().name = nameInput.value; scheduleSave({ actions: S.actions }) })
    // Colour picker: a swatch row plus a free-form hex field.
    const colorRow = el('div', { class: 'swatches' })
    for (const c of RAINBOW) {
      const sw = el('button', {
        class: 'swatch-btn' + ((a.color || '').toLowerCase() === c ? ' on' : ''),
        type: 'button',
        title: c,
        style: `background:${c}`
      })
      sw.addEventListener('click', () => { cur().color = c; saveNow({ actions: S.actions }).then(render) })
      colorRow.appendChild(sw)
    }
    const colorInput = el('input', { type: 'text', value: a.color || RAINBOW[0], maxlength: '9' })
    onEnterSave(colorInput)
    colorInput.addEventListener('input', () => { cur().color = colorInput.value; scheduleSave({ actions: S.actions }) })
    body.appendChild(el('div', { class: 'grid2' }, [
      el('div', { class: 'field' }, [el('label', { text: '名称' }), nameInput]),
      el('div', { class: 'field' }, [el('label', { text: '颜色' }), colorRow, colorInput])
    ]))
    if (!isLocal) {
      // One field: the prompt IS the action. There is no type selector — the built-in
      // presets simply ship with their instruction already written here.
      const ta = el('textarea', { placeholder: '给模型的提示词', rows: '4' })
      ta.value = a.prompt || ''
      ta.addEventListener('input', () => { cur().prompt = ta.value; scheduleSave({ actions: S.actions }) })
      body.appendChild(el('div', { class: 'field' }, [el('label', { text: '提示词' }), ta]))
      const provSel = el('select')
      provSel.appendChild(el('option', { value: '', text: '使用默认模型' }))
      for (const p of S.providers || []) { const op = el('option', { value: p.id, text: (p.name || p.model || '未命名') + (p.model ? ' · ' + p.model : '') }); if (p.id === a.providerId) op.selected = true; provSel.appendChild(op) }
      provSel.addEventListener('change', () => { cur().providerId = provSel.value || undefined; saveNow({ actions: S.actions }).then(render) })
      const thinkSel = el('select')
      thinkSel.appendChild(el('option', { value: 'inherit', text: '跟随默认' }))
      for (const lv of THINKING_LEVELS) { const op = el('option', { value: lv.value, text: lv.label }); if ((a.thinkingLevel || 'inherit') === lv.value) op.selected = true; thinkSel.appendChild(op) }
      thinkSel.addEventListener('change', () => { cur().thinkingLevel = thinkSel.value; saveNow({ actions: S.actions }).then(render) })
      body.appendChild(el('div', { class: 'grid2' }, [
        el('div', { class: 'field' }, [el('label', { text: '模型' }), provSel]),
        el('div', { class: 'field' }, [el('label', { text: '思考强度' }), thinkSel])
      ]))
    }
    card.appendChild(body)
    list.appendChild(card)
  })
  if (!(S.actions || []).length) list.appendChild(el('div', { class: 'empty-state', text: '还没有动作。' }))
  content.appendChild(section('动作', list))
  content.appendChild(el('div', {}, [el('button', {
    class: 'btn primary',
    text: '添加动作',
    onclick: () => {
      S.actions = S.actions || []
      S.actions.push({ id: genId('act'), name: '新动作', color: RAINBOW[S.actions.length % RAINBOW.length], builtin: 'custom', prompt: '', enabled: true, thinkingLevel: 'inherit' })
      saveNow({ actions: S.actions }).then(render)
    }
  })]))
}
function moveAction(index, delta) {
  const arr = S.actions, target = index + delta
  if (target < 0 || target >= arr.length) return
  const [it] = arr.splice(index, 1); arr.splice(target, 0, it)
  saveNow({ actions: arr }).then(render)
}

// ---- models ----
function renderModels() {
  const frag = document.createDocumentFragment()
  frag.appendChild(section('思考设置',
    selectRow('默认思考强度', S.thinkingLevel || 'auto', THINKING_LEVELS, (v) => saveNow({ thinkingLevel: v }).then(render))))
  const list = el('div', { class: 'card-list' })
  ;(S.providers || []).forEach((p) => {
    // Resolve the CURRENT provider object on every edit (stale references used to discard
    // every keystroke after the first).
    const cur = () => (S.providers || []).find((x) => x.id === p.id) || p
    const isDefault = (S.defaultProviderId || '') === p.id || (!S.defaultProviderId && (S.providers || [])[0] === p)
    const card = el('div', { class: 'item-card' })
    card.appendChild(el('div', { class: 'item-head' }, [
      el('span', { class: 'swatch', style: 'background:var(--accent)' }),
      el('div', { class: 'grow' }, [
        el('div', { class: 'nm' }, [el('span', { text: p.name || '未命名' }), isDefault ? el('span', { class: 'badge', text: '默认' }) : null]),
        el('div', { class: 'sub', text: (p.model || '未设置') + ' · ' + (p.apiBase || '未设置') })
      ]),
      el('div', { class: 'item-actions' }, [
        isDefault ? null : el('button', { class: 'btn sm', text: '设为默认', onclick: () => saveNow({ defaultProviderId: p.id }).then(() => { toast('已设为默认'); render() }) }),
        el('button', { class: 'btn sm', text: '测试', onclick: (e) => testProvider(cur(), e.target) }),
        el('button', { class: 'btn sm danger', text: '删除', onclick: () => { S.providers = S.providers.filter((x) => x.id !== p.id); const patch = { providers: S.providers, defaultProviderId: S.defaultProviderId === p.id ? '' : S.defaultProviderId }; saveNow(patch).then(render) } })
      ])
    ]))
    const body = el('div', { class: 'item-body' })
    const presetRow = el('div', { class: 'toolbar-line' })
    for (const preset of PROVIDER_PRESETS) {
      presetRow.appendChild(el('button', { class: 'btn sm', text: preset.label, onclick: () => { const q = cur(); q.apiBase = preset.apiBase; q.model = preset.model; if (!q.name) q.name = preset.label; saveNow({ providers: S.providers }).then(render) } }))
    }
    body.appendChild(presetRow)
    const mk = (label, key, type) => {
      const i = onEnterSave(el('input', { type: type || 'text', value: p[key] != null ? p[key] : '' }))
      i.addEventListener('input', () => {
        cur()[key] = i.value
        scheduleSave({ providers: S.providers })
      })
      return el('div', { class: 'field' }, [el('label', { text: label }), i])
    }
    body.appendChild(el('div', { class: 'grid2' }, [mk('名称', 'name'), mk('API 地址', 'apiBase')]))
    body.appendChild(el('div', { class: 'grid2' }, [mk('API Key', 'apiKey', 'password'), mk('模型', 'model')]))
    const tempInput = onEnterSave(el('input', { type: 'number', value: p.temperature != null ? p.temperature : 0.7, min: '0', max: '2', step: '0.1' }))
    tempInput.addEventListener('input', () => { cur().temperature = parseFloat(tempInput.value); scheduleSave({ providers: S.providers }) })
    const capSel = el('select')
    for (const c of THINKING_CAPS) { const op = el('option', { value: c.value, text: c.label }); if ((p.thinking || 'auto') === c.value) op.selected = true; capSel.appendChild(op) }
    capSel.addEventListener('change', () => { cur().thinking = capSel.value; saveNow({ providers: S.providers }).then(render) })
    body.appendChild(el('div', { class: 'grid2' }, [
      el('div', { class: 'field' }, [el('label', { text: '温度' }), tempInput]),
      el('div', { class: 'field' }, [el('label', { text: '思考能力' }), capSel])
    ]))

    // Extra request headers, one "Name: Value" per line (some gateways require them).
    const hdrTa = el('textarea', { placeholder: 'X-Custom-Header: value', rows: '2' })
    hdrTa.value = (p.headers || []).map((h) => `${h.name}: ${h.value}`).join('\n')
    hdrTa.addEventListener('input', () => {
      const pairs = hdrTa.value
        .split('\n')
        .map((line) => {
          const i = line.indexOf(':')
          if (i < 0) return null
          const name = line.slice(0, i).trim()
          const value = line.slice(i + 1).trim()
          return name ? { name, value } : null
        })
        .filter(Boolean)
      cur().headers = pairs
      scheduleSave({ providers: S.providers })
    })
    body.appendChild(el('div', { class: 'field' }, [el('label', { text: '请求头' }), hdrTa]))
    card.appendChild(body)
    list.appendChild(card)
  })
  if (!(S.providers || []).length) list.appendChild(el('div', { class: 'empty-state', text: '还没有模型提供商。' }))
  frag.appendChild(section('模型提供商', list))
  frag.appendChild(el('div', {}, [el('button', {
    class: 'btn primary',
    text: '添加提供商',
    onclick: () => {
      S.providers = S.providers || []
      S.providers.push({ id: genId('prov'), name: '', apiBase: '', apiKey: '', model: '', temperature: 0.7, thinking: 'auto', headers: [] })
      saveNow({ providers: S.providers }).then(render)
    }
  })]))
  content.appendChild(frag)
}
async function testProvider(p, btn) {
  const original = btn.textContent
  btn.textContent = '测试中'
  btn.disabled = true
  const res = await invoke('test_provider', { provider: p })
  btn.disabled = false
  btn.textContent = original
  toast(res.ok ? '连接成功：' + res.message : '连接失败：' + res.message)
}

// ---- about ----
async function renderAbout() {
  const info = await invoke('app_info')
  content.appendChild(section('关于', el('div', { class: 'kv' }, [
    el('div', { class: 'k', text: '版本' }), el('div', { class: 'v', text: info.version }),
    el('div', { class: 'k', text: '选词引擎' }), el('div', { class: 'v', text: info.hookRunning ? '运行中' : '已停止' }),
    el('div', { class: 'k', text: '运行时' }), el('div', { class: 'v', text: 'Tauri 2 · WebView2' }),
    el('div', { class: 'k', text: '配置文件' }), el('div', { class: 'v mono', text: info.userDataPath })
  ])))
  content.appendChild(section('说明', el('div', {}, [
    el('a', { class: 'link', text: 'Cherry Studio (GitHub)', onclick: () => invoke('open_external', { url: 'https://github.com/CherryHQ/cherry-studio' }) })
  ])))
  content.appendChild(section('重置', el('div', {}, [el('button', { class: 'btn danger', text: '恢复默认设置', onclick: async () => { if (!confirm('确定恢复默认设置？所有动作与模型配置将被清除。')) return; S = await invoke('settings_reset'); toast('已恢复默认设置'); render() } })])))
}

// ---- shell ----
function render() {
  document.querySelectorAll('.nav').forEach((n) => n.classList.toggle('active', n.dataset.tab === tab))
  content.innerHTML = ''
  if (tab === 'general') renderGeneral()
  else if (tab === 'actions') renderActions()
  else if (tab === 'models') renderModels()
  else if (tab === 'about') renderAbout()
}
document.querySelectorAll('.nav').forEach((n) => n.addEventListener('click', () => { tab = n.dataset.tab; render() }))

// Never lose an edit because the window went away.
window.addEventListener('blur', () => { void flushSave() })
window.addEventListener('beforeunload', () => { void flushSave() })

;(async function boot() {
  S = await invoke('settings_get')
  render()
})()

// Settings can change while this window is open (e.g. the tray toggles 启用) or right after
// one of our own saves. Refresh the view only when the user is not interacting with it:
// re-rendering under the cursor would move focus and the caret out of the active field.
listen('settings-changed', (e) => {
  S = e.payload
  if (!pendingPatch && !document.hasFocus()) render()
})
})()
