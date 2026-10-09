import winreg
keys = ["picoSun.photo","picoSun.raw","picoSun.hdr","picoSun.anim",
        "picosun2.photo","picosun2.raw","picosun2.hdr","picosun2.anim"]
for k in keys:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, f"Software\\Classes\\{k}\\shell\\open\\command") as h:
            v = winreg.QueryValue(h, None)
            print(f"[{k}] => {v}")
    except FileNotFoundError:
        pass
    except OSError as e:
        print(f"[{k}] ERR {e}")
