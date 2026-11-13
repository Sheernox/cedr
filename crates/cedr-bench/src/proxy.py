"""CONNECT-only benchmark egress. No request headers, bodies or destinations are logged."""
import ipaddress
import os
import select
import socket
import socketserver

ALLOWED = frozenset(os.environ['BENCH_HOSTS'].split(','))


def allowed(authority: str) -> bool:
    host, sep, port = authority.rpartition(':')
    return bool(sep) and port == '443' and host in ALLOWED


class Tunnel(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        self.request.settimeout(15)
        data = b''
        try:
            while b'\r\n\r\n' not in data and len(data) < 16384:
                chunk = self.request.recv(4096)
                if not chunk:
                    return
                data += chunk
            head, extra = data.split(b'\r\n\r\n', 1)
            method, authority, protocol = head.split(b'\r\n', 1)[0].decode('ascii').split()
            if method != 'CONNECT' or protocol != 'HTTP/1.1' or not allowed(authority):
                self.request.sendall(b'HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n')
                return
            host = authority.rsplit(':', 1)[0]
            addresses = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
            if not addresses or any(not ipaddress.ip_address(a[4][0]).is_global for a in addresses):
                self.request.sendall(b'HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n')
                return
            family, kind, proto, _, addr = addresses[0]
            with socket.socket(family, kind, proto) as upstream:
                upstream.settimeout(30)
                upstream.connect(addr)
                self.request.sendall(b'HTTP/1.1 200 Connection Established\r\n\r\n')
                if extra:
                    upstream.sendall(extra)
                while True:
                    ready, _, _ = select.select([self.request, upstream], [], [], 120)
                    if not ready:
                        return
                    for source in ready:
                        chunk = source.recv(65536)
                        if not chunk:
                            return
                        (upstream if source is self.request else self.request).sendall(chunk)
        except (OSError, ValueError, UnicodeError):
            return


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


if __name__ == '__main__':
    with Server(('0.0.0.0', 3128), Tunnel) as server:
        server.serve_forever()
