import time,urllib.request
for attempt in range(300):
    try:
        with urllib.request.urlopen('http://127.0.0.1:18080/api/drivers',timeout=1) as response:
            if response.status==200:break
    except OSError:time.sleep(.1)
else:raise SystemExit('River API did not become ready')
