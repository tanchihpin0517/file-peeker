可以。下面是一個完整、可實作的流程，假設：

```text
本機：Mac
遠端：my-server
遠端程式：remote-server
遠端程式會監聽 127.0.0.1 的隨機 port
```

目標是：

```text
1. 建立一條 SSH master connection
2. 透過同一條 connection 啟動遠端 server
3. 從 server stdout 取得遠端 port
4. 再透過同一條 connection 建立 local port forwarding
5. 本機程式連 local port
```

OpenSSH 的 `ControlMaster` 允許多個 `ssh` process 經由本機 control socket，共用同一條已認證的 SSH network connection。([OpenBSD Manual Pages][1])

---

## 整體架構

```text
Local machine
┌───────────────────────────────────────────────┐
│                                               │
│ launcher                                      │
│   │                                           │
│   ├─ ssh master process                       │
│   │      │                                    │
│   │      ├─ Unix control socket               │
│   │      │    /tmp/my-ssh-control             │
│   │      │                                    │
│   │      └════════ SSH TCP connection ════════╪════╗
│   │                                           │    ║
│   ├─ ssh process: 啟動 remote-server           │    ║
│   │      └─ 經 control socket 使用 master      │    ║
│   │                                           │    ║
│   └─ ssh process: 建立 forwarding              │    ║
│          └─ 經 control socket 使用 master      │    ║
│                                               │    ║
│ 127.0.0.1:LOCAL_PORT                          │    ║
└───────────────┬───────────────────────────────┘    ║
                │                                    ║
                └══ forwarding channel ══════════════╣
                                                     ║
Remote machine                                       ║
┌────────────────────────────────────────────────────╨─┐
│ sshd                                                 │
│   ├─ session channel → remote-server                  │
│   └─ direct-tcpip channel → 127.0.0.1:REMOTE_PORT     │
│                                                      │
│ remote-server                                        │
│   └─ listen 127.0.0.1:REMOTE_PORT                     │
└──────────────────────────────────────────────────────┘
```

底層只有一條：

```text
local ssh master ↔ remote sshd
```

TCP/SSH connection。

---

# 第一階段：建立 SSH master

先決定 control socket：

```bash
CONTROL_SOCKET="$HOME/.ssh/cm-my-server"
```

建立 master：

```bash
ssh \
  -M \
  -S "$CONTROL_SOCKET" \
  -o ControlPersist=10m \
  -Nf \
  my-server
```

參數：

```text
-M
```

讓這個 `ssh` 成為 master。

```text
-S "$CONTROL_SOCKET"
```

指定本機 Unix domain socket。後面的 `ssh` process 會連這個 socket。

```text
-o ControlPersist=10m
```

即使目前沒有 active session，master 仍保留十分鐘。

```text
-N
```

master 本身不執行遠端命令。

```text
-f
```

完成認證後進入背景。

這一步會：

```text
1. TCP connect 到遠端 SSH port
2. SSH key exchange
3. host key 驗證
4. password/public-key authentication
5. 建立加密 SSH transport
6. 建立本機 control socket
7. master 進入背景
```

你只會在這一步輸入密碼。

OpenSSH 說明中，`ControlMaster` 會監聽 `ControlPath` 指定的 socket；後續 session 會嘗試重用 master 的 network connection。([OpenBSD Manual Pages][1])

可以檢查 master：

```bash
ssh -S "$CONTROL_SOCKET" -O check my-server
```

可能輸出：

```text
Master running (pid=12345)
```

---

# 第二階段：用同一連線啟動遠端 server

現在執行另一個 `ssh` process：

```bash
ssh \
  -S "$CONTROL_SOCKET" \
  -T \
  my-server \
  'remote-server --host 127.0.0.1 --port 0'
```

這個新 `ssh` process 不會重新連遠端。

它的流程是：

```text
ssh process B
    │
    │ connect()
    ▼
$CONTROL_SOCKET
    │
    ▼
ssh master process
    │
    │ 在既有 SSH connection 裡建立 session channel
    ▼
remote sshd
    │
    │ exec request
    ▼
remote-server --host 127.0.0.1 --port 0
```

`-T` 是不分配 pseudo-terminal。這很重要，因為你希望 stdout 是乾淨、可解析的 protocol，而不是 terminal output。

---

## 遠端 server 選擇 port

遠端 Rust server 可以：

```rust
use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;

    println!("PORT={}", address.port());

    // 非常重要：立即 flush，讓本機讀得到。
    use std::io::Write;
    std::io::stdout().flush()?;

    for connection in listener.incoming() {
        let stream = connection?;
        // 處理連線
        drop(stream);
    }

    Ok(())
}
```

當它執行：

```rust
TcpListener::bind(("127.0.0.1", 0))
```

kernel 會選一個可用 port，例如：

```text
127.0.0.1:43827
```

然後 server stdout：

```text
PORT=43827
```

---

# 第三階段：從 SSH stdout 取得 port

最簡單的 shell 做法：

```bash
SERVER_OUTPUT="$(
  ssh \
    -S "$CONTROL_SOCKET" \
    -T \
    my-server \
    'remote-server --host 127.0.0.1 --port 0'
)"
```

但這裡有一個問題：

> 如果 `remote-server` 持續執行，command substitution 會一直等到 server 結束。

所以通常要讓 server 在遠端背景執行，並把啟動資訊寫到檔案，或者維持該 SSH process、非同步讀取 stdout。

## 作法 A：SSH process 持續存在

這比較接近 VS Code。

本機 launcher 啟動 child process：

```text
ssh -S control my-server remote-server
```

然後持續讀 child stdout：

```text
PORT=43827
```

讀到 port 後，不關閉這個 child；它同時負責承載 server 的 stdout/stderr 和生命週期。

Rust 本機 launcher 概念：

```rust
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

fn main() -> std::io::Result<()> {
    let control_socket = "/tmp/my-ssh-control";

    let mut child = Command::new("ssh")
        .args([
            "-S",
            control_socket,
            "-T",
            "my-server",
            "remote-server --host 127.0.0.1 --port 0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();

    let remote_port = loop {
        let Some(line) = lines.next() else {
            return Err(std::io::Error::other(
                "server exited before reporting port",
            ));
        };

        let line = line?;

        if let Some(value) = line.strip_prefix("PORT=") {
            let port: u16 = value.parse().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid port: {error}"),
                )
            })?;

            break port;
        }
    };

    println!("remote server uses port {remote_port}");

    // 接下來建立 forwarding。
    Ok(())
}
```

這個 child `ssh` process 雖然還活著，但它仍然只是：

```text
一個 multiplexed session client
```

底層遠端 TCP connection 仍由 master 擁有。

---

## 作法 B：遠端 daemonize

也可以讓遠端 server 背景化：

```bash
ssh -S "$CONTROL_SOCKET" my-server '
  nohup remote-server --host 127.0.0.1 --port 0 \
    > ~/.cache/remote-server/startup.log \
    2>&1 &
'
```

然後再執行另一個 multiplexed command：

```bash
ssh -S "$CONTROL_SOCKET" my-server \
  'sed -n "s/^PORT=//p" ~/.cache/remote-server/startup.log | head -1'
```

但這會需要：

* PID file
* startup log
* readiness check
* 清理舊 process
* 避免同時啟動兩份

因此 launcher 直接持有 server session 通常更乾淨。

---

# 第四階段：用同一條 master 建立 forwarding

假設遠端 server 回傳：

```text
REMOTE_PORT=43827
```

現在需要本地：

```text
127.0.0.1:LOCAL_PORT
```

forward 到：

```text
remote 127.0.0.1:43827
```

例如選本地 port `55123`：

```bash
ssh \
  -S "$CONTROL_SOCKET" \
  -O forward \
  -L 127.0.0.1:55123:127.0.0.1:43827 \
  my-server
```

這個 command 不建立新遠端 SSH connection。

它做的是：

```text
ssh process C
    │
    │ connect to local ControlPath
    ▼
ssh master
    │
    │ 新增 local forwarding 規則
    ▼
listen on local 127.0.0.1:55123
```

`ssh -O forward` 是向已存在的 multiplexing master 發送控制命令。

之後：

```bash
curl http://127.0.0.1:55123
```

或你的 Rust client：

```text
connect 127.0.0.1:55123
```

資料流會是：

```text
local application
    │ TCP connect
    ▼
127.0.0.1:55123
    │
    ▼
SSH master local listener
    │
    │ open direct-tcpip SSH channel
    ▼
existing SSH encrypted connection
    │
    ▼
remote sshd
    │ TCP connect
    ▼
remote 127.0.0.1:43827
```

---

# 本地 port 也可以動態選擇

避免自己挑 `55123` 造成競爭條件，可以要求 OpenSSH 使用本地 port `0`：

```bash
LOCAL_PORT="$(
  ssh \
    -S "$CONTROL_SOCKET" \
    -O forward \
    -L 127.0.0.1:0:127.0.0.1:"$REMOTE_PORT" \
    my-server
)"
```

OpenSSH 會讓 kernel 動態配置 listen port，然後把分配結果印到 stdout。官方 `ssh(1)` 文件指出，port 為 `0` 時可動態配置，搭配 `-O forward` 時，配置出的 port 會輸出至 stdout。([OpenBSD Manual Pages][2])

例如：

```text
55123
```

於是：

```bash
echo "Local endpoint: 127.0.0.1:$LOCAL_PORT"
```

這比先自己找空 port 更安全，因為：

```text
bind(port=0)
```

和占用該 port 是原子操作，不會有「檢查完後被別人搶走」的 race。

---

# 完整 shell 範例

這是一個簡化版本：

```bash
#!/usr/bin/env bash
set -euo pipefail

HOST="my-server"
CONTROL_SOCKET="$HOME/.ssh/cm-${HOST}"

cleanup() {
    if [[ -n "${LOCAL_PORT:-}" && -n "${REMOTE_PORT:-}" ]]; then
        ssh \
          -S "$CONTROL_SOCKET" \
          -O cancel \
          -L "127.0.0.1:${LOCAL_PORT}:127.0.0.1:${REMOTE_PORT}" \
          "$HOST" 2>/dev/null || true
    fi

    if [[ -n "${SERVER_SSH_PID:-}" ]]; then
        kill "$SERVER_SSH_PID" 2>/dev/null || true
    fi

    ssh -S "$CONTROL_SOCKET" -O exit "$HOST" 2>/dev/null || true
}

trap cleanup EXIT INT TERM

# 1. 建立 master；這裡可能要求一次密碼。
ssh \
  -M \
  -S "$CONTROL_SOCKET" \
  -o ControlPersist=10m \
  -Nf \
  "$HOST"

# 2. 建立 FIFO，用來接遠端 server stdout。
OUTPUT_FIFO="$(mktemp -u)"
mkfifo "$OUTPUT_FIFO"

# 3. 用相同 master 啟動 server。
ssh \
  -S "$CONTROL_SOCKET" \
  -T \
  "$HOST" \
  'exec remote-server --host 127.0.0.1 --port 0' \
  >"$OUTPUT_FIFO" &

SERVER_SSH_PID=$!

# 4. 讀到 PORT=...。
while IFS= read -r line; do
    case "$line" in
        PORT=*)
            REMOTE_PORT="${line#PORT=}"
            break
            ;;
    esac
done <"$OUTPUT_FIFO"

rm -f "$OUTPUT_FIFO"

if [[ ! "$REMOTE_PORT" =~ ^[0-9]+$ ]]; then
    echo "Invalid remote port: $REMOTE_PORT" >&2
    exit 1
fi

echo "Remote server: 127.0.0.1:$REMOTE_PORT"

# 5. 在同一 master connection 新增 forwarding。
LOCAL_PORT="$(
  ssh \
    -S "$CONTROL_SOCKET" \
    -O forward \
    -L "127.0.0.1:0:127.0.0.1:${REMOTE_PORT}" \
    "$HOST"
)"

echo "Local endpoint: 127.0.0.1:$LOCAL_PORT"

# 6. 使用 forwarding。
curl "http://127.0.0.1:$LOCAL_PORT"

# 保持 launcher 運行。
wait "$SERVER_SSH_PID"
```

---

# 這裡實際用了幾個 process？

本機可能有：

```text
Process A：ssh master
Process B：啟動 remote-server 的 ssh client
Process C：ssh -O forward，執行完即退出
Process D：你的 launcher
```

但是只有 Process A 持有：

```text
TCP socket → remote sshd
```

Process B 和 Process C 都經由：

```text
ControlPath Unix socket
```

要求 Process A 建立新的 SSH channel。

所以：

```text
4 個本機 process
≠ 4 條 SSH network connection
```

實際上是：

```text
1 條 TCP connection
1 次 SSH authentication
多條 SSH channel
```

---

# SSH channel 的分工

啟動 server 時是 session channel：

```text
channel type: session
request: exec
command: remote-server --port 0
```

forwarding 有連線進來時，master 每次建立一條：

```text
channel type: direct-tcpip
destination host: 127.0.0.1
destination port: 43827
```

因此 forwarding 不是把原本啟動 server 的 session channel 拿來傳資料；它是在**同一條 SSH transport 裡另外建立 channel**。

```text
SSH transport
├── channel 0：remote-server stdin/stdout/stderr
├── channel 1：第一條 forwarded TCP connection
├── channel 2：第二條 forwarded TCP connection
└── channel 3：其他遠端命令
```

---

# 實作時最重要的細節

遠端 server 應該先完成 `bind()` 與 `listen()`，再輸出 port：

```rust
let listener = TcpListener::bind(("127.0.0.1", 0))?;
let port = listener.local_addr()?.port();

println!("PORT={port}");
stdout().flush()?;
```

不要先挑 port、輸出 port，然後才 bind。否則中間可能被其他 process 搶走。

另外，啟動 server 時建議：

```bash
ssh -T ...
```

避免 PTY 修改 buffering、插入控制字元或改變 signal 行為。

最終時序如下：

```text
Launcher       SSH master       Remote sshd       Remote server
   │                │                │                  │
   │── start ──────▶│                │                  │
   │   password     │── authenticate▶│                  │
   │                │◀──── OK ───────│                  │
   │                │                                   
   │── exec via ───▶│── session ────▶│── start ───────▶│
   │ ControlPath    │   channel      │                  │
   │                │                │                  │ bind :0
   │◀════════════ PORT=43827 ═══════════════════════════│
   │                │                │                  │
   │── -O forward ─▶│                │                  │
   │                │ listen local   │                  │
   │◀── 55123 ──────│                │                  │
   │                │                │                  │
 App connect         │                │                  │
   │── localhost ──▶│── direct- ────▶│── TCP connect ─▶│
   │    :55123      │   tcpip        │ 127.0.0.1:43827  │
```

這就是「一次認證、一條 SSH transport、先啟動 server 並發現 port，再動態加入 forwarding」的完整流程。

[1]: https://man.openbsd.org/ssh_config?utm_source=chatgpt.com "ssh_config(5) - OpenBSD manual pages"
[2]: https://man.openbsd.org/OpenBSD-7.7/ssh.1?utm_source=chatgpt.com "ssh(1) - OpenBSD manual pages"
