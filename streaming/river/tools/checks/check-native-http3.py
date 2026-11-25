"""Verify negotiated QUIC HTTP/3 using a trusted fixture certificate."""
import asyncio
import sys

from aioquic.asyncio import QuicConnectionProtocol, connect
from aioquic.h3.connection import H3_ALPN, H3Connection
from aioquic.h3.events import DataReceived, HeadersReceived
from aioquic.quic.configuration import QuicConfiguration
from aioquic.quic.events import ProtocolNegotiated


class Client(QuicConnectionProtocol):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.http = H3Connection(self._quic)
        self.responses = {}
        self.alpn = None

    def quic_event_received(self, event):
        if isinstance(event, ProtocolNegotiated):
            self.alpn = event.alpn_protocol
        for item in self.http.handle_event(event):
            if isinstance(item, (DataReceived, HeadersReceived)) and item.stream_id in self.responses:
                self.responses[item.stream_id].put_nowait(item)

    async def request(self, authority, path):
        stream = self._quic.get_next_available_stream_id()
        response = self.responses[stream] = asyncio.Queue()
        self.http.send_headers(stream, [
            (b':method', b'GET'), (b':scheme', b'https'), (b':authority', authority),
            (b':path', path), (b'origin', b'https://viewer.example'),
        ], end_stream=True)
        self.transmit()
        headers, body = {}, bytearray()
        while True:
            event = await asyncio.wait_for(response.get(), timeout=8)
            if isinstance(event, HeadersReceived):
                headers.update(event.headers)
            else:
                body.extend(event.data)
            if event.stream_ended:
                return headers, body


async def main(port, cert):
    config = QuicConfiguration(is_client=True, alpn_protocols=H3_ALPN,
                               server_name='localhost', cafile=cert)
    async with connect('127.0.0.1', int(port), configuration=config, create_protocol=Client) as client:
        assert client.alpn == 'h3', client.alpn
        headers, body = await client.request(('localhost:'+port).encode(), b'/openapi.json')
        assert headers[b':status'] == b'200' and b'"openapi"' in body
        assert headers[b'access-control-allow-origin'] == b'https://viewer.example'
        print('HTTP/3 negotiated over UDP with verified certificate and CORS')


asyncio.run(asyncio.wait_for(main(*sys.argv[1:]), timeout=16))
