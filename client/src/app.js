// DSH Client loading page — talks to the Rust shell through the Tauri IPC bridge.
const { invoke } = window.__TAURI__.core
const { listen } = window.__TAURI__.event

const el = {
  status: document.getElementById('status'),
  detail: document.getElementById('detail'),
  actions: document.getElementById('actions'),
  retry: document.getElementById('retry'),
  checkUpdate: document.getElementById('check-update'),
  version: document.getElementById('version'),
}

function setStatus(text, kind) {
  el.status.textContent = text
  el.status.className = 'status' + (kind ? ` ${kind}` : '')
}

async function refresh() {
  const s = await invoke('get_status')
  el.version.textContent = `内核 dsh v${s.kernel_version} · 客户端 v${s.app_version}`
  switch (s.phase) {
    case 'ready':
      setStatus(`已就绪 · http://127.0.0.1:${s.port}`, 'ready')
      el.actions.classList.add('hidden')
      break
    case 'error':
      setStatus('启动失败', 'error')
      el.detail.textContent = s.error || ''
      el.actions.classList.remove('hidden')
      break
    default:
      setStatus(statusText(s.phase))
      el.actions.classList.add('hidden')
  }
}

function statusText(phase) {
  const map = {
    idle: '等待启动…',
    installing: '正在解压内核…',
    starting: '正在启动 dsh 服务…',
    restarting: 'dsh 进程异常，正在重启…',
    stopped: '已停止',
    dev: '开发模式',
  }
  return map[phase] || phase
}

async function main() {
  el.retry.addEventListener('click', async () => {
    setStatus('正在重试…')
    el.actions.classList.add('hidden')
    await invoke('retry_kernel')
  })
  el.checkUpdate.addEventListener('click', async () => {
    el.checkUpdate.disabled = true
    setStatus('正在检查更新…')
    const r = await invoke('check_updates')
    if (r === 'none') {
      setStatus('已是最新版本')
      el.actions.classList.remove('hidden')
    }
    el.checkUpdate.disabled = false
  })

  await listen('kernel-status', (e) => {
    const s = e.payload
    if (s.phase === 'ready') {
      setStatus(`已就绪 · http://127.0.0.1:${s.port}`, 'ready')
      el.actions.classList.add('hidden')
    } else if (s.phase === 'error') {
      setStatus('启动失败', 'error')
      el.detail.textContent = s.error || ''
      el.actions.classList.remove('hidden')
    } else {
      setStatus(statusText(s.phase))
      el.detail.textContent = s.version ? `内核 dsh v${s.version}` : ''
    }
  })

  await listen('update-available', () => {
    setStatus('发现新版本，正在后台下载…')
  })
  await listen('update-installed', () => {
    setStatus('更新完成，正在重启应用…')
  })

  await refresh()
}

main()
