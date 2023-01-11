import asyncio
import aiohttp

async def main():
    async with aiohttp.ClientSession() as session:
        async with session.ws_connect('wss://harmony.bobobot.cf') as ws:
            print(await ws.receive())

            await ws.send_json({'op': 'identify', 'token': 'MTYxMTAyMTA1OTQ0MDY0.NjE0NTU1OTk1.fyDYPEEDin_h3vhqsEQ7n0qj5c8PNp78affhtaa42A8'})
            print(await ws.receive())


asyncio.run(main())
