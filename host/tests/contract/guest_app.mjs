import http from 'node:http';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
let hits = 0;
fs.writeFileSync('/tmp/guest-hits', '0');
const server = http.createServer((req, res) => {
  if (req.url === '/health') { res.end('ok'); return; }
  fs.writeFileSync('/tmp/guest-hits', String(++hits));
  if (req.url === '/stream') {
    res.writeHead(200, { 'Content-Type': 'text/plain', 'Cache-Control': 'public, max-age=86400' });
    res.write('open\n'); const timer = setInterval(() => res.write('open\n'), 100);
    req.on('close', () => clearInterval(timer)); return;
  }
  if (req.url === '/sw.js') { res.writeHead(200, { 'Content-Type': 'application/javascript' }); res.end("self.addEventListener('fetch', () => {});"); return; }
  if (req.url === '/') { res.writeHead(200, { 'Content-Type': 'text/html', 'Cache-Control': 'public, max-age=86400' }); res.end('<!doctype html><title>Guest fixture</title><h1>Guest fixture</h1><p>Authenticated application content.</p>'); return; }
  res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'public, max-age=86400' });
  res.end(JSON.stringify({ url: req.url, headers: req.headers, pr: process.env.VERCELIFY_PR_NUMBER }));
});
server.on('upgrade', (req, socket) => {
  if (req.url !== '/ws' || !req.headers['sec-websocket-key']) { socket.end('HTTP/1.1 404 Not Found\r\n\r\n'); return; }
  const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
  socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + '\r\n\r\n');
  let pending = Buffer.alloc(0);
  socket.on('data', chunk => {
    pending = Buffer.concat([pending, chunk]);
    while (pending.length >= 2) {
      const opcode = pending[0] & 15, masked = !!(pending[1] & 128); let size = pending[1] & 127, at = 2;
      if (size === 126) { if (pending.length < 4) return; size = pending.readUInt16BE(2); at = 4; }
      if (size === 127 || size > 4096) { socket.destroy(); return; }
      if (pending.length < at + (masked ? 4 : 0) + size) return;
      const mask = masked ? pending.subarray(at, at + 4) : null; if (masked) at += 4;
      const payload = Buffer.from(pending.subarray(at, at + size)); if (mask) for (let i = 0; i < size; i++) payload[i] ^= mask[i % 4];
      pending = pending.subarray(at + size);
      if (opcode === 8) { socket.end(Buffer.from([0x88, 0])); return; }
      if (size < 126 && (opcode === 1 || opcode === 9)) socket.write(Buffer.concat([Buffer.from([opcode === 9 ? 0x8a : 0x81, size]), payload]));
    }
  });
  socket.on('error', () => {});
});
server.listen(8080, '0.0.0.0');
