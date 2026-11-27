"""通过 WebView2 的 DevTools 协议 (CDP) 在页面里执行 JS, 验证前端桥接是否正常。

用法:
    python tools/probe_gui.py <exe 路径> [--port 9333]

会打印:
    - window.__TAURI__ 是否存在
    - window.qt / QWebChannel 是否就位
    - backend.scan_icons() 的实际返回 (前 200 字符)
"""

import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

# 极简 WebSocket 客户端 (只够发 CDP 命令, 不引第三方依赖)
import base64
import os
import socket
import struct


def ws_connect(url: str):
    """建立 WebSocket 连接, 返回 (socket, 剩余 buffer)。"""
    assert url.startswith("ws://")
    rest = url[5:]
    hostport, _, path = rest.partition("/")
    host, _, port = hostport.partition(":")
    port = int(port or 80)

    s = socket.create_connection((host, port), timeout=15)
    key = base64.b64encode(os.urandom(16)).decode()
    req = (
        f"GET /{path} HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n\r\n"
    )
    s.sendall(req.encode())

    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = s.recv(4096)
        if not chunk:
            raise RuntimeError("WebSocket 握手失败")
        buf += chunk
    header, _, leftover = buf.partition(b"\r\n\r\n")
    if b"101" not in header.split(b"\r\n")[0]:
        raise RuntimeError(f"WebSocket 握手被拒: {header[:200]!r}")
    return s, bytearray(leftover)


def ws_send(s, buf: bytearray, payload: str) -> None:
    """发送一个文本帧 (客户端必须加掩码)。"""
    data = payload.encode()
    mask = os.urandom(4)
    masked = bytes(b ^ mask[i % 4] for i, b in enumerate(data))

    header = bytearray([0x81])  # FIN + text
    n = len(data)
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header += struct.pack(">H", n)
    else:
        header.append(0x80 | 127)
        header += struct.pack(">Q", n)
    header += mask
    s.sendall(bytes(header) + masked)


def ws_recv(s, buf: bytearray, timeout: float = 15.0) -> str:
    """接收一个完整文本帧。"""
    s.settimeout(timeout)

    def need(n: int) -> None:
        while len(buf) < n:
            chunk = s.recv(65536)
            if not chunk:
                raise RuntimeError("WebSocket 连接已关闭")
            buf.extend(chunk)

    need(2)
    b1, b2 = buf[0], buf[1]
    opcode = b1 & 0x0F
    masked = b2 & 0x80
    length = b2 & 0x7F
    offset = 2
    if length == 126:
        need(4)
        length = struct.unpack(">H", bytes(buf[2:4]))[0]
        offset = 4
    elif length == 127:
        need(10)
        length = struct.unpack(">Q", bytes(buf[2:10]))[0]
        offset = 10
    if masked:
        need(offset + 4)
        mask = bytes(buf[offset : offset + 4])
        offset += 4
    need(offset + length)
    payload = bytes(buf[offset : offset + length])
    del buf[: offset + length]
    if masked:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    if opcode == 0x8:
        raise RuntimeError("WebSocket 被关闭")
    return payload.decode("utf-8", "replace")


def cdp_eval(ws_url: str, expr: str, timeout: float = 20.0) -> str:
    """在页面里执行 JS, 返回结果的字符串形式。"""
    s, buf = ws_connect(ws_url)
    try:
        ws_send(
            s,
            buf,
            json.dumps(
                {
                    "id": 1,
                    "method": "Runtime.evaluate",
                    "params": {
                        "expression": expr,
                        "awaitPromise": True,
                        "returnByValue": True,
                    },
                }
            ),
        )
        deadline = time.time() + timeout
        while time.time() < deadline:
            msg = json.loads(ws_recv(s, buf, timeout=max(1.0, deadline - time.time())))
            if msg.get("id") == 1:
                res = msg.get("result", {})
                if "exceptionDetails" in res:
                    return f"<exception: {res['exceptionDetails']}>"
                result = res.get("result", {})
                val = result.get("value")
                if val is None:
                    # 可能是 undefined / object(无 value) —— CDP 对复杂对象会走 objectId
                    desc = result.get("description")
                    if desc:
                        return f"<undefined> {desc}"
                    return f"<{result.get('type')}>"
                if isinstance(val, (dict, list)):
                    return json.dumps(val, ensure_ascii=False)
                # 注意: 不要把 socket 变量 `s` 覆盖掉 (finally 里要 close)
                text = str(val)
                # 单行化, 便于阅读
                return text.replace("\n", " ")[:600]
        return "<timeout>"
    finally:
        s.close()


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    port = 9333
    for a in sys.argv[1:]:
        if a.startswith("--port="):
            port = int(a.split("=", 1)[1])

    exe = Path(args[0]).resolve()

    env = dict(os.environ)
    env["QTWEBENGINE_DISABLE_SANDBOX"] = "1"
    env["WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"] = f"--remote-debugging-port={port}"

    print(f"启动: {exe}")
    proc = subprocess.Popen([str(exe)], cwd=str(exe.parent), env=env)

    try:
        tabs = None
        for _ in range(40):
            time.sleep(0.5)
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{port}/json", timeout=3
                ) as r:
                    tabs = json.load(r)
                if tabs:
                    break
            except Exception:
                continue

        if not tabs:
            print("[FAIL] 连不上 WebView2 DevTools")
            return 1

        page = next(
            (t for t in tabs if t.get("type") == "page" and t.get("webSocketDebuggerUrl")),
            None,
        )
        if not page:
            print(f"[FAIL] 没找到页面 tab: {tabs}")
            return 1

        ws = page["webSocketDebuggerUrl"]
        print(f"页面: {page.get('title')}\n")

        checks = [
            ("window.__TAURI__ 存在", "typeof window.__TAURI__"),
            ("__TAURI__.core.invoke 存在", "typeof (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke)"),
            ("__TAURI__.event.listen 存在", "typeof (window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen)"),
            ("QWebChannel 兼容层已装", "typeof window.QWebChannel"),
            ("qt.webChannelTransport 存在", "typeof (window.qt && window.qt.webChannelTransport)"),
            ("backend 对象可用", "typeof (window.backend)"),
        ]
        for label, expr in checks:
            try:
                r = cdp_eval(ws, expr)
            except Exception as e:
                r = f"<error: {e}>"
            ok = r.strip('"') == "object" or r.strip('"') == "function"
            print(f"  {'OK  ' if ok else 'FAIL'}  {label}: {r}")

        print()
        print("=== 实际调用 scan_icons ===")
        try:
            r = cdp_eval(
                ws,
                """(async () => {
                    try {
                        const r = await window.__TAURI__.core.invoke('scan_icons');
                        return 'OK len=' + (r ? r.length : 0) + ' head=' + String(r).slice(0, 120);
                    } catch (e) {
                        return 'ERR ' + e;
                    }
                })()""",
            )
            print(f"  {r}")
        except Exception as e:
            print(f"  <error: {e}>")

        return 0
    finally:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass


if __name__ == "__main__":
    raise SystemExit(main())
