import asyncio
import aiohttp

async def main():
    async with aiohttp.ClientSession() as session:
        async with session.ws_connect('wss://harmony.bobobot.cf') as ws:
            print(await ws.receive())

            await ws.send_json({'op': 'identify', 'token': 'NDEwOTUwMDM5MjQ4OTA1.MTU2NzY1MDIzMg.BeG6bsfi-2q5CEv1RvYLMgNFFg5RLeQLOGAE_-9pKbI'})
            async for m in ws:
                print(m)


asyncio.run(main())
