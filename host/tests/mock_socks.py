"""A SOCKS5 proxy for tests: every CONNECT goes to TARGET (host:port) and
the requested destination is logged. Usage: mock_socks.py PORTFILE LOG TARGET"""
import socket
import struct
import sys
import threading

port_file, log_file, target = sys.argv[1], sys.argv[2], sys.argv[3]
thost, tport = target.rsplit(":", 1)


def pump(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d:
                break
            b.sendall(d)
    except OSError:
        pass
    finally:
        try:
            b.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def client(c):
    ver, n = c.recv(2)
    c.recv(n)
    c.sendall(b"\x05\x00")
    _, cmd, _, atyp = c.recv(4)
    if atyp == 1:
        host = socket.inet_ntoa(c.recv(4))
    elif atyp == 3:
        host = c.recv(c.recv(1)[0]).decode()
    else:
        host = socket.inet_ntop(socket.AF_INET6, c.recv(16))
    port = struct.unpack(">H", c.recv(2))[0]
    with open(log_file, "a") as f:
        f.write(f"CONNECT {host}:{port}\n")
    up = socket.create_connection((thost, int(tport)))
    c.sendall(b"\x05\x00\x00\x01" + b"\0" * 6)
    threading.Thread(target=pump, args=(c, up), daemon=True).start()
    pump(up, c)


s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0))
s.listen(16)
with open(port_file, "w") as f:
    f.write(str(s.getsockname()[1]))
while True:
    conn, _ = s.accept()
    threading.Thread(target=client, args=(conn,), daemon=True).start()
