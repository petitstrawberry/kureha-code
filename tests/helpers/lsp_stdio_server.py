"""Deterministic subprocess used only by the Rust stdio transport tests."""
import json
import os
import sys
import time


def send(message, fragmented=False):
    body = json.dumps(message, ensure_ascii=False).encode("utf-8")
    frame = b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body
    if fragmented:
        for byte in frame:
            sys.stdout.buffer.write(bytes([byte]))
            sys.stdout.buffer.flush()
    else:
        sys.stdout.buffer.write(frame)
        sys.stdout.buffer.flush()


def receive():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            raise EOFError()
        if line == b"\r\n":
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    body = sys.stdin.buffer.read(length)
    assert len(body) == length
    return json.loads(body)


mode = sys.argv[1]
if mode == "malformed":
    sys.stdout.buffer.write(b"Content-Length: 1\r\n\r\n{")
    sys.stdout.buffer.flush()
elif mode == "truncated":
    sys.stdout.buffer.write(b"Content-Length: 10\r\n\r\n{}")
    sys.stdout.buffer.flush()
elif mode == "stderr_exit":
    sys.stderr.write("x" * 100_000 + "\nfixture fatal diagnostic: 名前\n")
    sys.stderr.flush()
    sys.exit(9)
elif mode == "no_read":
    send({"jsonrpc": "2.0", "method": "fixture/ready", "params": {"pid": os.getpid()}})
    time.sleep(60)
elif mode == "flood":
    for i in range(400):
        send({"jsonrpc": "2.0", "method": "fixture/flood", "params": {"i": i}})
    time.sleep(60)
else:
    # Force the backend to drain more than a pipe buffer before it can reply.
    sys.stderr.write("x" * 100_000 + "\nfixture stderr tail\n")
    sys.stderr.flush()
    while True:
        message = receive()
        method = message.get("method")
        if method == "fixture/echo":
            send({"jsonrpc": "2.0", "method": "fixture/notice", "params": {"text": "😀名前"}}, True)
            send({"jsonrpc": "2.0", "id": message["id"], "result": message["params"]}, True)
        elif method == "fixture/error":
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32800, "message": "cancelled", "data": {"detail": 7}}})
        elif method == "fixture/serverRequests":
            requests = [
                ("configuration-string-id", "workspace/configuration", {"items": [{"section": "rust-analyzer"}, {}]}),
                (-7, "fixture/unsupported", {}),
                (0, "workspace/applyEdit", {"edit": {"changes": {}}}),
                ("progress", "window/workDoneProgress/create", {"token": "t"}),
                ("folders", "workspace/workspaceFolders", {}),
                ("prompt", "window/showMessageRequest", {"type": 3, "message": "test"}),
                ("invalid-config", "workspace/configuration", {}),
                ("registration", "client/registerCapability", {"registrations": []}),
            ]
            replies = []
            for request_id, request_method, params in requests:
                send({"jsonrpc": "2.0", "id": request_id, "method": request_method, "params": params})
                reply = receive()
                assert reply["id"] == request_id
                replies.append(reply)
            send({"jsonrpc": "2.0", "method": "fixture/replies", "params": {"replies": replies}})
        elif method == "fixture/exit":
            sys.exit(0)
