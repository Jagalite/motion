"""Loopback-only SMTP sink for explicitly selected Motion test deliveries.

python3 qualification/auth-signin/mail.py --mailbox /path/to/isolated/mail
Prints its ephemeral port; messages remain private local .eml fixture files.
"""
import argparse
import asyncio
import json
import os
import pathlib
import uuid


async def serve(mailbox, port):
    async def connection(reader, writer):
        async def reply(value):
            writer.write(value.encode("ascii") + b"\r\n")
            await writer.drain()

        try:
            await reply("220 localhost Motion isolated test mailbox")
            message = bytearray()
            data = False
            while line := await asyncio.wait_for(reader.readline(), timeout=30):
                if len(line) > 16384 or len(message) > 65536:
                    await reply("552 fixture message too large")
                    break
                if data:
                    if line == b".\r\n":
                        destination = mailbox / f"{uuid.uuid4().hex}.eml"
                        descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                        with os.fdopen(descriptor, "wb") as stream:
                            stream.write(message)
                        message.clear()
                        data = False
                        await reply("250 stored in isolated mailbox")
                    else:
                        message.extend(line)
                elif line.upper().startswith((b"EHLO", b"HELO")):
                    await reply("250-localhost")
                    await reply("250 8BITMIME")
                elif line.upper().startswith(b"DATA"):
                    data = True
                    await reply("354 send fixture message")
                elif line.upper().startswith(b"QUIT"):
                    await reply("221 closing")
                    break
                elif line.upper().startswith((b"MAIL FROM:", b"RCPT TO:", b"RSET", b"NOOP")):
                    await reply("250 accepted")
                else:
                    await reply("502 unsupported fixture command")
        except (TimeoutError, ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            await writer.wait_closed()

    server = await asyncio.start_server(connection, "127.0.0.1", port, limit=16385)
    print(json.dumps({"host": "127.0.0.1", "port": server.sockets[0].getsockname()[1],
                      "mailbox": str(mailbox)}), flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mailbox", type=pathlib.Path, required=True)
    parser.add_argument("--port", type=int, default=0)
    arguments = parser.parse_args()
    if not 0 <= arguments.port <= 65535:
        parser.error("port must be in 0..65535")
    arguments.mailbox.mkdir(parents=True, mode=0o700, exist_ok=True)
    try:
        asyncio.run(serve(arguments.mailbox.resolve(), arguments.port))
    except KeyboardInterrupt:
        pass
