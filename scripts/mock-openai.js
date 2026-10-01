'use strict'
/**
 * Mock OpenAI-compatible endpoint for local verification.
 *
 * Streams a canned reply (reasoning first, then content) and LOGS the message roles of
 * every request, so multi-turn behaviour can be checked:
 *
 *   node scripts/mock-openai.js 8799
 *
 * Turn 1 should log roles=system,user ; a follow-up should log roles=system,user,assistant,user.
 */
const http = require('http')

const port = Number(process.argv[2] || 8799)

const server = http.createServer((req, res) => {
  if (req.method === 'POST' && req.url.endsWith('/chat/completions')) {
    let body = ''
    req.on('data', (c) => (body += c))
    req.on('end', () => {
      let roles = []
      try {
        const j = JSON.parse(body)
        roles = (j.messages || []).map((m) => m.role)
      } catch {
        roles = ['<unparsable>']
      }
      console.log('REQUEST roles=' + roles.join(','))

      res.writeHead(200, {
        'Content-Type': 'text/event-stream; charset=utf-8',
        'Cache-Control': 'no-cache',
        Connection: 'keep-alive'
      })
      const reasoning = ['用户', '想要', '翻译', '……']
      const content = ['这是', '一个', '来自', '模拟', '模型的', '**流式**', '回复。']
      const all = [
        ...reasoning.map((c) => ({ field: 'reasoning_content', content: c })),
        ...content.map((c) => ({ field: 'content', content: c }))
      ]
      let i = 0
      const timer = setInterval(() => {
        if (i >= all.length) {
          res.write('data: [DONE]\n\n')
          clearInterval(timer)
          res.end()
          return
        }
        const item = all[i]
        res.write(
          'data: ' +
            JSON.stringify({
              id: 'mock',
              object: 'chat.completion.chunk',
              model: 'mock-model',
              choices: [{ index: 0, delta: { [item.field]: item.content }, finish_reason: null }]
            }) +
            '\n\n'
        )
        i++
      }, 40)
    })
    return
  }
  res.writeHead(200, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify({ ok: true }))
})

server.listen(port, '127.0.0.1', () => console.log('MOCK_LISTENING ' + port))
