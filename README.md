# picoSun2

A fast, minimal photo viewer for Windows. Built with Rust and egui.

[English](#picosun2) · [فارسی](#picosun2-فارسی)

![picoSun2](screenshot.png)

## Features

**Fast photo loading**
- Full resolution, no downscaling
- Multi-threaded RAW demosaic (26MP ARW in ~4s)
- Instant paging on cached photos (LRU-8)
- Per-folder cache in a hidden `.p2cache/` folder — cold reload drops from 8.9s to 0.5s

**Filmstrip**
- Bottom strip shows all photos in the folder
- Current photo outlined in blue
- Placeholder tiles appear immediately, decode in the background
- Scroll the strip to page through photos
- Dedicated tile pool — thumbnails never starve the main photo decode

**Supported formats**
- Photos: JPG, PNG, GIF, WebP, BMP, TIFF, HEIC, AVIF, and 50+ more
- RAW: ARW, CR2, CR3, NEF, DNG, ORF, RAF, RW2, and 40+ more
- HDR: EXR, HDR, PFM, FITS
- Animated: GIF, APNG, WebP

**Navigation**
- Arrow keys or wheel to page
- Ctrl+wheel to zoom, drag to pan
- R to rotate, H to flip
- Ctrl+T toggles the filmstrip
- F for fullscreen

**Open with**
- Registered as OpenWith for all photo and RAW formats
- Never hijacks your default apps

## Install

Download `picoSun2-setup-2.0.0.exe` from the [latest release](https://github.com/Nim4a/picoSun2/releases/latest).

Or grab the portable `picosun2.exe` directly.

## Cache

The first time you open a photo, picoSun2 caches the decoded result as a PNG inside a hidden `.p2cache/` folder next to your photos. The next open is near-instant.

- One hidden folder per photo folder — your photo folder stays clean
- Stale entries are detected automatically (source file mtime + size check)
- Delete the `.p2cache/` folder anytime to reclaim space

## Build

Requires Rust and the w64devkit toolchain.

```
cargo build --release
```

The installer requires NSIS:

```
cd installer && makensis picoSun2.nsi
```

## License

MIT

---

## picoSun2 (فارسی)

نمایشگر عکس سریع و مینیمال برای ویندوز. با Rust و egui ساخته شده.

![picoSun2](screenshot.png)

### قابلیت‌ها

**لودینگ سریع عکس**
- رزولوشن کامل، بدون کاهش اندازه
- دیموزایک RAW چندنخی (ARW ۲۶ مگاپیکسل در ~۴ ثانیه)
- صفحه‌گذاری فوری روی عکس‌های کش‌شده (LRU-8)
- کش هر فولدر در فولدر پنهان `.p2cache/` — لود مجدد از ۸.۹ ثانیه به ۰.۵ ثانیه

**نوار پایین (فیلم‌استریپ)**
- نوار پایین همهٔ عکس‌های فولدر را نشان می‌دهد
- عکس فعلی با کادر آبی مشخص می‌شود
- تامبنیل‌های placeholder فوری ظاهر می‌شوند، decode در پس‌زمینه
- اسکرول روی نوار = جابجایی بین عکس‌ها
- pool اختصاصی تامبنیل — decode عکس اصلی هرگز گرسنه نمی‌ماند

**فرمت‌های پشتیبانی‌شده**
- عکس: JPG, PNG, GIF, WebP, BMP, TIFF, HEIC, AVIF و ۵۰+ فرمت دیگر
- RAW: ARW, CR2, CR3, NEF, DNG, ORF, RAF, RW2 و ۴۰+ فرمت دیگر
- HDR: EXR, HDR, PFM, FITS
- انیمیشن: GIF, APNG, WebP

**ناوبری**
- کلید جهت‌دار یا موس برای جابجایی
- Ctrl+موس برای زوم، درگ برای پن
- R چرخش، H آینه افقی
- Ctrl+T نمایش/مخفی کردن نوار پایین
- F فول‌اسکرین

**Open with**
- به‌عنوان OpenWith برای همهٔ فرمت‌های عکس و RAW ثبت می‌شود
- هرگز برنامهٔ پیش‌فرض شما را عوض نمی‌کند

### نصب

`picoSun2-setup-2.0.0.exe` را از [آخرین ریلیز](https://github.com/Nim4a/picoSun2/releases/latest) دانلود کنید.

یا مستقیماً `picosun2.exe` قابل‌حمل را بردارید.

### کش

بار اول که عکسی را باز می‌کنید، picoSun2 نتیجهٔ decode را به‌صورت PNG داخل فولدر پنهان `.p2cache/` کنار عکس‌های شما ذخیره می‌کند. باز کردن بعدی تقریباً فوری است.

- یک فولدر پنهان برای هر فولدر عکس — فولدر عکس‌های شما تمیز می‌ماند
- ورودی‌های قدیمی خودکار تشخیص داده می‌شوند (چک mtime و size فایل اصلی)
- هر وقت خواستید `.p2cache/` را پاک کنید تا فضا آزاد شود

### بیلد

نیاز به Rust و toolchain w64devkit دارد.

```
cargo build --release
```

نصب‌کننده نیاز به NSIS دارد:

```
cd installer && makensis picoSun2.nsi
```

### لایسنس

MIT
