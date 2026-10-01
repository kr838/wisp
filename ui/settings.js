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
  { label: 'OpenCode Go', apiBase: 'https://opencode.ai/zen/go/v1', model: 'deepseek-v4.1-flash', note: '自动附加 x-opencode-session' },
  { label: 'OpenCode Zen', apiBase: 'https://opencode.ai/zen/v1', model: 'deepseek-v4.1-flash', note: '自动附加 x-opencode-session' },
  { label: 'Ollama', apiBase: 'http://localhost:11434/v1', model: 'llama3.1' },
  { label: 'LM Studio', apiBase: 'http://localhost:1234/v1', model: 'local-model' }
]

const BUILTIN_OPTIONS = [
  { value: 'translate', label: '翻译（中英互译）' },
  { value: 'summarize', label: '总结要点' },
  { value: 'explain', label: '解释/讲解' },
  { value: 'rewrite', label: '润色改写' },
  { value: 'custom', label: '自定义提示词' },
  { value: 'copy', label: '复制（本地，不调用模型）' },
  { value: 'search', label: '搜索（用搜索引擎打开选中文字）' }
]
// Rainbow palette used to colour actions (replaces the old emoji icons).
const RAINBOW = [
  '#3b6fe0', '#7c3aed', '#c2410c', '#0f766e',
  '#be185d', '#4d7c0f', '#0e7490', '#9333ea'
]

const THINKING_LEVELS = [
  { value: 'auto', label: '自动（交给模型默认）' },
  { value: 'off', label: '关闭思考' },
  { value: 'low', label: '轻度思考' },
  { value: 'medium', label: '标准思考' },
  { value: 'high', label: '深度思考' }
]
const THINKING_CAPS = [
  { value: 'auto', label: '自动识别' },
  { value: 'on', label: '支持思考' },
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
// Edits auto-save (debounced), and the state is shown in the bar at the bottom so it is
// never ambiguous whether something was written. Enter, the 保存 button, and closing or
// leaving the window all force an immediate write.
let saveTimer = null
let pendingPatch = null

/// Transient feedback instead of a permanent status bar: saving is automatic, so the UI
/// only needs to confirm it happened (and shout if it failed).
function setSaveState(kind, text) {
  if (kind === 'error') {
    toast(text)
  } else if (kind === '' && text === '已保存') {
    toast('已保存')
  }
  // 'pending' / 'saving' are intentionally silent — the write follows a moment later.
}

/// Write any pending patch immediately.
async function flushSave() {
  clearTimeout(saveTimer)
  saveTimer = null
  if (!pendingPatch) {
    setSaveState('', '已保存')
    return
  }
  const patch = pendingPatch
  pendingPatch = null
  setSaveState('saving', '正在保存…')
  try {
    S = await invoke('settings_set', { patch })
    setSaveState('', '已保存')
  } catch (err) {
    // Put the patch back so a retry (or Enter) can try again.
    pendingPatch = Object.assign({}, patch, pendingPatch || {})
    setSaveState('error', '保存失败：' + (err && err.message ? err.message : err))
  }
}

function scheduleSave(patch) {
  pendingPatch = Object.assign({}, pendingPatch || {}, patch)
  setSaveState('pending', '有未保存的更改')
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
function section(title, desc, body) {
  return el('div', { class: 'section' }, [el('h2', { text: title }), desc ? el('p', { class: 'desc', text: desc }) : null, body])
}
function switchRow(label, sub, checked, onChange) {
  const sw = el('button', { class: 'switch' + (checked ? ' on' : ''), type: 'button' })
  sw.addEventListener('click', () => { const next = !sw.classList.contains('on'); sw.classList.toggle('on', next); onChange(next) })
  return el('div', { class: 'row' }, [el('div', { class: 'label' }, [label, sub ? el('small', { text: sub }) : null]), el('div', { class: 'control' }, [sw])])
}
function selectRow(label, sub, value, options, onChange) {
  const sel = el('select')
  for (const o of options) { const op = el('option', { value: o.value, text: o.label }); if (o.value === value) op.selected = true; sel.appendChild(op) }
  sel.addEventListener('change', () => onChange(sel.value))
  return el('div', { class: 'row' }, [el('div', { class: 'label' }, [label, sub ? el('small', { text: sub }) : null]), el('div', { class: 'control', style: 'min-width:240px' }, [sel])])
}
function iconBtn(text, title, disabled, onClick) {
  return el('button', { class: 'btn sm', text, title, style: disabled ? 'opacity:.4;pointer-events:none' : '', onclick: onClick })
}
function builtinLabel(b) { const f = BUILTIN_OPTIONS.find((o) => o.value === b); return f ? f.label : '' }

// ---- general ----
function renderGeneral() {
  content.appendChild(section('Wisp', '开启后，在任意软件中选中文字即会弹出悬浮操作条。', el('div', {}, [
    switchRow('启用 Wisp', '关闭后将不再监听划词', S.enabled, (v) => saveNow({ enabled: v }).then(render)),
    selectRow('触发方式', '「选中即弹出」适合多数场景；「按住 Ctrl」可避免误触发', S.triggerMode,
      [{ value: 'selected', label: '选中即弹出' }, { value: 'ctrlkey', label: '按住 Ctrl 键显示' }],
      (v) => saveNow({ triggerMode: v }).then(render)),
    switchRow('结果窗口跟随悬浮条', '关闭则在屏幕中央显示结果', S.followToolbar, (v) => saveNow({ followToolbar: v })),
    switchRow('剪贴板辅助取词', '无法直接读取选中内容时，用 Ctrl+C 兜底（会覆盖剪贴板）', S.clipboardFallback, (v) => saveNow({ clipboardFallback: v }))
  ])))
  content.appendChild(section('应用过滤', '控制哪些程序需要/不需要划词。默认已排除资源管理器、截图、图像/视频编辑等软件。', el('div', {}, [
    selectRow('过滤模式', '', S.filterMode,
      [{ value: 'default', label: '默认（推荐）' }, { value: 'blacklist', label: '黑名单：列表内不触发' }, { value: 'whitelist', label: '白名单：仅列表内触发' }],
      (v) => saveNow({ filterMode: v }).then(render)),
    S.filterMode !== 'default'
      ? el('div', { class: 'field' }, [
          el('label', { text: '程序名列表（每行一个，如 chrome.exe）' }),
          (() => {
            const ta = el('textarea', { placeholder: 'chrome.exe\nnotepad.exe' })
            ta.value = (S.filterList || []).join('\n')
            ta.addEventListener('input', () => scheduleSave({ filterList: ta.value.split('\n').map((s) => s.trim()).filter(Boolean) }))
            return ta
          })()
        ])
      : el('p', { class: 'hint-inline', text: '默认模式已内置常用软件排除列表。' })
  ])))
  content.appendChild(section('外观与搜索', '', el('div', {}, [
    selectRow('悬浮条主题', '', S.theme, [{ value: 'light', label: '浅色' }, { value: 'dark', label: '深色' }], (v) => saveNow({ theme: v }).then(render)),
    selectRow('窗口材质', 'Mica 只采样桌面壁纸（几乎不模糊）；Acrylic 会实时模糊窗口背后的内容（真正的毛玻璃）', S.material || 'mica', [
      { value: 'mica', label: 'Mica（云母，Win11 默认）' },
      { value: 'mica_alt', label: 'Mica Alt（对比度略高）' },
      { value: 'acrylic', label: 'Acrylic（真毛玻璃，实时模糊）' },
      { value: 'none', label: '无（纯色不透明）' }
    ], (v) => saveNow({ material: v })),
    selectRow('材质明暗', '决定材质用浅色还是深色版本；「跟随系统」最省心', S.materialDark || 'auto', [
      { value: 'auto', label: '跟随系统' },
      { value: 'light', label: '始终浅色' },
      { value: 'dark', label: '始终深色' }
    ], (v) => saveNow({ materialDark: v })),
    el('div', { class: 'row' }, [
      el('div', { class: 'label' }, ['材质不透明度', el('small', { text: '数值越低，材质越明显；越低文字对比也越弱' })]),
      el('div', { class: 'control', style: 'min-width:240px' }, [
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
      el('label', { text: '搜索动作使用的搜索引擎地址（{query} 为占位符）' }),
      (() => { const i = el('input', { type: 'text', value: S.searchUrl || '' }); i.addEventListener('input', () => scheduleSave({ searchUrl: i.value.trim() })); return i })()
    ])
  ])))
}

// ---- actions ----
function renderActions() {
  const list = el('div', { class: 'card-list' })
  ;(S.actions || []).forEach((a, index) => {
    // Same stale-reference guard as the providers list: a save replaces `S` with fresh
    // objects, so every edit must resolve the current one by id.
    const cur = () => (S.actions || []).find((x) => x.id === a.id) || a
    const isCopy = a.builtin === 'copy'
    // Built-ins that run locally and use no prompt/model.
    const isLocal = a.builtin === 'copy' || a.builtin === 'search'
    const card = el('div', { class: 'item-card' + (a.enabled === false ? ' is-off' : '') })
    card.appendChild(el('div', { class: 'item-head' }, [
      el('span', { class: 'swatch', style: `background:${a.color || RAINBOW[0]}` }),
      el('div', { class: 'grow' }, [
        el('div', { class: 'nm' }, [el('span', { text: a.name || '未命名' }), isCopy ? el('span', { class: 'badge', text: '本地' }) : null, a.enabled === false ? el('span', { class: 'badge danger', text: '已停用' }) : null]),
        el('div', { class: 'sub', text: a.builtin === 'custom' ? (a.prompt || '未填写提示词') : builtinLabel(a.builtin) })
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
      el('div', { class: 'field' }, [el('label', { text: '颜色（用于区分动作）' }), colorRow, colorInput])
    ]))
    const typeSel = el('select')
    for (const o of BUILTIN_OPTIONS) { const op = el('option', { value: o.value, text: o.label }); if (o.value === a.builtin) op.selected = true; typeSel.appendChild(op) }
    typeSel.addEventListener('change', () => { cur().builtin = typeSel.value; saveNow({ actions: S.actions }).then(render) })
    body.appendChild(el('div', { class: 'field' }, [el('label', { text: '动作类型' }), typeSel]))
    if (!isLocal) {
      const ta = el('textarea', { placeholder: a.builtin === 'custom' ? '输入给模型的系统提示词，例如：把文本翻译成日语' : '附加要求（可选）' })
      ta.value = a.prompt || ''
      ta.addEventListener('input', () => { cur().prompt = ta.value; scheduleSave({ actions: S.actions }) })
      body.appendChild(el('div', { class: 'field' }, [el('label', { text: a.builtin === 'custom' ? '提示词' : '附加要求（追加到内置提示词后，可选）' }), ta]))
      const provSel = el('select')
      provSel.appendChild(el('option', { value: '', text: '使用默认模型' }))
      for (const p of S.providers || []) { const op = el('option', { value: p.id, text: (p.name || p.model || '未命名') + (p.model ? ' · ' + p.model : '') }); if (p.id === a.providerId) op.selected = true; provSel.appendChild(op) }
      provSel.addEventListener('change', () => { cur().providerId = provSel.value || undefined; saveNow({ actions: S.actions }).then(render) })
      body.appendChild(el('div', { class: 'field' }, [el('label', { text: '指定模型（可选）' }), provSel]))
      const thinkSel = el('select')
      thinkSel.appendChild(el('option', { value: 'inherit', text: '跟随默认' }))
      for (const lv of THINKING_LEVELS) { const op = el('option', { value: lv.value, text: lv.value === 'auto' ? '自动' : lv.label }); if ((a.thinkingLevel || 'inherit') === lv.value) op.selected = true; thinkSel.appendChild(op) }
      thinkSel.addEventListener('change', () => { cur().thinkingLevel = thinkSel.value; saveNow({ actions: S.actions }).then(render) })
      body.appendChild(el('div', { class: 'field' }, [el('label', { text: '思考强度（覆盖全局设置）' }), thinkSel]))
    }
    card.appendChild(body)
    list.appendChild(card)
  })
  if (!(S.actions || []).length) list.appendChild(el('div', { class: 'empty-state', text: '还没有动作，点击下方按钮添加。' }))
  content.appendChild(section('动作列表', '这些动作会显示在弹出的悬浮条上，点击后用 AI 处理选中文本。', list))
  content.appendChild(el('div', {}, [el('button', { class: 'btn primary', text: '＋ 添加动作', onclick: () => { S.actions = S.actions || []; S.actions.push({ id: genId('act'), name: '自定义动作', color: RAINBOW[S.actions.length % RAINBOW.length], builtin: 'custom', prompt: '', enabled: true, thinkingLevel: 'inherit' }); saveNow({ actions: S.actions }).then(render) } })]))
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
  frag.appendChild(section('思考设置', '控制模型是否进行「思考 / 推理」及强度。仅对支持思考的模型生效；关闭可显著加快翻译等简单任务。',
    selectRow('默认思考强度', '「自动」交给模型自行决定，不发送任何思考参数', S.thinkingLevel || 'auto', THINKING_LEVELS, (v) => saveNow({ thinkingLevel: v }).then(render))))
  const list = el('div', { class: 'card-list' })
  ;(S.providers || []).forEach((p) => {
    // Resolve the CURRENT provider object on every edit.
    //
    // Saving triggers a `settings-changed` broadcast that replaces `S` with fresh objects
    // from Rust, so a captured `p` goes stale: the next keystroke would mutate the dead
    // object while sending the fresh array — silently discarding what was typed (this is
    // why only the first field of a provider used to persist).
    const cur = () => (S.providers || []).find((x) => x.id === p.id) || p
    const isDefault = (S.defaultProviderId || '') === p.id || (!S.defaultProviderId && (S.providers || [])[0] === p)
    const card = el('div', { class: 'item-card' })
    card.appendChild(el('div', { class: 'item-head' }, [
      el('span', { class: 'swatch', style: 'background:var(--accent)' }),
      el('div', { class: 'grow' }, [
        el('div', { class: 'nm' }, [el('span', { text: p.name || '未命名提供商' }), isDefault ? el('span', { class: 'badge', text: '默认' }) : null]),
        el('div', { class: 'sub', text: (p.model || '未设置模型') + ' · ' + (p.apiBase || '未设置地址') })
      ]),
      el('div', { class: 'item-actions' }, [
        el('button', { class: 'btn sm', text: '设为默认', onclick: () => saveNow({ defaultProviderId: p.id }).then(() => { toast('已设为默认模型'); render() }) }),
        el('button', { class: 'btn sm', text: '测试连接', onclick: (e) => testProvider(cur(), e.target) }),
        el('button', { class: 'btn sm danger', text: '删除', onclick: () => { S.providers = S.providers.filter((x) => x.id !== p.id); const patch = { providers: S.providers, defaultProviderId: S.defaultProviderId === p.id ? '' : S.defaultProviderId }; saveNow(patch).then(render) } })
      ])
    ]))
    const body = el('div', { class: 'item-body' })
    const presetRow = el('div', { class: 'toolbar-line' }, [el('span', { class: 'hint-inline', text: '快速填充：' })])
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
    body.appendChild(el('div', { class: 'grid2' }, [mk('名称', 'name'), mk('API 地址（Base URL）', 'apiBase')]))
    body.appendChild(el('div', { class: 'grid2' }, [mk('API Key', 'apiKey', 'password'), mk('模型名称', 'model')]))
    const tempInput = onEnterSave(el('input', { type: 'number', value: p.temperature != null ? p.temperature : 0.7, min: '0', max: '2', step: '0.1' }))
    tempInput.addEventListener('input', () => { cur().temperature = parseFloat(tempInput.value); scheduleSave({ providers: S.providers }) })
    const capSel = el('select')
    for (const c of THINKING_CAPS) { const op = el('option', { value: c.value, text: c.label }); if ((p.thinking || 'auto') === c.value) op.selected = true; capSel.appendChild(op) }
    capSel.addEventListener('change', () => { cur().thinking = capSel.value; saveNow({ providers: S.providers }).then(render) })
    body.appendChild(el('div', { class: 'grid2' }, [
      el('div', { class: 'field' }, [el('label', { text: '温度' }), tempInput]),
      el('div', { class: 'field' }, [el('label', { text: '思考能力（该模型是否支持思考）' }), capSel])
    ]))

    // Extra request headers, one "Name: Value" per line. Some gateways require them
    // (OpenCode needs x-opencode-session, which is filled automatically; this is for
    // anything else a provider may demand).
    const hdrTa = el('textarea', { placeholder: '每行一个，例如：\nX-Custom-Header: value', rows: '2' })
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
    body.appendChild(el('div', { class: 'field' }, [
      el('label', { text: '自定义请求头（每行一个 Name: Value）' }),
      hdrTa
    ]))
    card.appendChild(body)
    list.appendChild(card)
  })
  if (!(S.providers || []).length) list.appendChild(el('div', { class: 'empty-state', text: '还没有模型提供商。添加一个 OpenAI 兼容接口即可使用翻译/总结等 AI 动作。' }))
  frag.appendChild(section('模型提供商', '支持任何 OpenAI 兼容接口（OpenAI、DeepSeek、Moonshot、智谱、硅基流动、Ollama、LM Studio 等）。API Key 仅保存在本机配置文件中。', list))
  frag.appendChild(el('div', {}, [el('button', { class: 'btn primary', text: '＋ 添加模型提供商', onclick: () => { S.providers = S.providers || []; S.providers.push({ id: genId('prov'), name: '', apiBase: '', apiKey: '', model: '', temperature: 0.7, thinking: 'auto', headers: [] }); saveNow({ providers: S.providers }).then(render) } })]))
  content.appendChild(frag)
}
async function testProvider(p, btn) {
  const original = btn.textContent
  btn.textContent = '测试中…'; btn.disabled = true
  const res = await invoke('test_provider', { provider: p })
  btn.disabled = false; btn.textContent = original
  toast(res.ok ? '连接成功：' + res.message : '连接失败：' + res.message)
}

// ---- about ----
async function renderAbout() {
  const info = await invoke('app_info')
  content.appendChild(section('关于', '', el('div', { class: 'kv' }, [
    el('div', { class: 'k', text: '应用' }), el('div', { class: 'v', text: info.name }),
    el('div', { class: 'k', text: '版本' }), el('div', { class: 'v', text: info.version }),
    el('div', { class: 'k', text: '选词引擎' }), el('div', { class: 'v', text: info.hookRunning ? 'selection-hook 原生引擎（运行中）' : 'selection-hook 原生引擎（已停止）' }),
    el('div', { class: 'k', text: '运行时' }), el('div', { class: 'v', text: 'Tauri 2 · WebView2' }),
    el('div', { class: 'k', text: '配置文件' }), el('div', { class: 'v mono' }, [el('span', { text: info.userDataPath })])
  ])))
  content.appendChild(section('说明', '本应用的划词能力提取自开源项目 Cherry Studio（MIT）所使用的 selection-hook 选词引擎；界面与外壳为 Tauri 重写，安装包体积约为原 Electron 版的十分之一。', el('div', {}, [
    el('a', { class: 'link', text: '→ 访问 Cherry Studio (GitHub)', onclick: () => invoke('open_external', { url: 'https://github.com/CherryHQ/cherry-studio' }) })
  ])))
  content.appendChild(section('快捷操作', '', el('div', {}, [el('button', { class: 'btn danger', text: '恢复默认设置', onclick: async () => { if (!confirm('确定恢复默认设置？所有动作与模型配置将被清除。')) return; S = await invoke('settings_reset'); toast('已恢复默认设置'); render() } })])))
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
listen('settings-changed', (e) => { S = e.payload })

// Never lose an edit because the window went away (no explicit save button needed).
window.addEventListener('blur', () => { void flushSave() })
window.addEventListener('beforeunload', () => { void flushSave() })

;(async function boot() {
  S = await invoke('settings_get')
  render()
})()
})()
