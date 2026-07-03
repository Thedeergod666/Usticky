"""Generate Usticky icons.

设计（沿用 Musage generate_icons.py 框架，字母换 U）：
- 主图标：白底圆角矩形 + 加粗 U（anchor="mm" 居中，留 padding 避免 macOS 看着偏大）
- 托盘底图（tray-base.png）：与主图标一致
- ICO：每个尺寸**原生渲染**（不降采样）—— 避免 Windows 模糊
- ICNS：macOS 上用 iconutil 拼真 .icns；其它平台 PNG fallback

跟 Musage 视觉差异（避免同时开两个 app 时搞混）：
- 字母 U（不是 M）
- 主色保留白底黑字（极简，跟系统托盘 / dock 风格一致）
"""
import sys, os
sys.stdout.reconfigure(encoding="utf-8")

from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src-tauri", "icons")
os.makedirs(OUT, exist_ok=True)

# 配色：白底 + 黑色加粗 U + 黑色细环
BG = (255, 255, 255, 255)
RING = (0, 0, 0, 200)
FG = (0, 0, 0, 255)

# 画布外圈 padding：macOS HIG 模板留 ~7%，跟 VSCode/WPS 在 dock 上视觉密度对齐
ICON_PADDING_RATIO = 0.07
# 字号占边长比例
U_SCALE = 0.50
# Ring 装饰：相对白底边距
RING_MARGIN = 0.08
RING_STROKE = 1 / 48


def find_font(size: int):
    """返回 (font, is_bold)。优先 Arial Black / Heavy，失败兜底 regular + stroke。"""
    bold_paths = [
        # Windows
        "C:/Windows/Fonts/ariblk.ttf",
        "C:/Windows/Fonts/arialbd.ttf",
        "C:/Windows/Fonts/segoeuib.ttf",
        "C:/Windows/Fonts/calibrib.ttf",
        # macOS
        "/System/Library/Fonts/Supplemental/Arial Black.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/Library/Fonts/Arial Bold.ttf",
        # Linux
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    ]
    for path in bold_paths:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size), True
            except Exception:
                pass

    regular_paths = [
        "C:/Windows/Fonts/seguiemj.ttf",
        "C:/Windows/Fonts/segoeui.ttf",
        "C:/Windows/Fonts/arial.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/SFNS.ttf",
        "/Library/Fonts/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]
    for path in regular_paths:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size), False
            except Exception:
                pass

    return None, False


def make_icon(size: int) -> Image.Image:
    """生成 Usticky 图标（指定尺寸原生渲染）。

    Layout（≥32）：dock 风格 —— 7% padding + 圆角白底 + ring + U
    Layout（≤24）：任务栏风格 —— 无 padding + 大 U（笔画粗、边缘清，不带 ring）
    """
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    is_small = size <= 24
    if is_small:
        pad = 0
        sq_size = size
        r = max(1, int(sq_size * 0.10))
        u_scale = 0.66
    else:
        pad = int(size * ICON_PADDING_RATIO)
        sq_size = size - 2 * pad
        r = int(sq_size * 0.225)
        u_scale = U_SCALE

    # 圆角白底
    d.rounded_rectangle(
        [(pad, pad), (pad + sq_size - 1, pad + sq_size - 1)],
        radius=r, fill=BG,
    )

    # Ring 装饰：仅 ≥32 画
    if RING_MARGIN > 0 and size >= 32:
        ring_offset = int(sq_size * RING_MARGIN)
        rx0 = pad + ring_offset
        ry0 = pad + ring_offset
        rx1 = pad + sq_size - 1 - ring_offset
        ry1 = pad + sq_size - 1 - ring_offset
        d.ellipse(
            [(rx0, ry0), (rx1, ry1)],
            outline=RING, width=max(1, int(size * RING_STROKE)),
        )

    # 中心 "U" —— anchor="mm" 真正像素级居中
    if size >= 16:
        font, is_bold = find_font(int(size * u_scale))
        if font is not None:
            if is_bold:
                d.text((size / 2, size / 2), "U", font=font, fill=FG, anchor="mm")
            else:
                # 兜底：stroke 模拟 Black 粗体
                sw = max(1, int(size * 0.06))
                d.text(
                    (size / 2, size / 2), "U", font=font, fill=FG,
                    stroke_width=sw, stroke_fill=FG, anchor="mm",
                )
                d.text((size / 2, size / 2), "U", font=font, fill=FG, anchor="mm")
    return img


# ── 1. PNG 各尺寸 ──
png_targets = [
    (32, "32x32.png"),
    (128, "128x128.png"),
    (256, "128x128@2x.png"),
]
for size, name in png_targets:
    make_icon(size).save(os.path.join(OUT, name))
    print(f"[ok] {name}")

# tray-base.png
make_icon(32).save(os.path.join(OUT, "tray-base.png"))
print("[ok] tray-base.png")

# ── 2. icon.ico（多尺寸，每个尺寸原生渲染）──
ico_sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
ico_images = [make_icon(s) for s, _ in ico_sizes]
ico_images_sorted = sorted(ico_images, key=lambda img: -img.size[0])
ico_images_sorted[0].save(
    os.path.join(OUT, "icon.ico"),
    format="ICO",
    append_images=ico_images_sorted[1:],
)
print(f"[ok] icon.ico (native sizes: {[img.size for img in ico_images_sorted]})")

# ── 3. icon.png (master 1024) ──
make_icon(1024).save(os.path.join(OUT, "icon.png"))
print("[ok] icon.png (1024x1024 master)")

# ── 4. icon.icns —— macOS 上用 iconutil 拼真 .icns ──
import subprocess, shutil, tempfile

icns_path = os.path.join(OUT, "icon.icns")
if sys.platform == "darwin" and shutil.which("iconutil"):
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "icon.iconset")
        os.makedirs(iconset)
        sizes = [
            (16, "icon_16x16.png"),
            (32, "icon_16x16@2x.png"),
            (32, "icon_32x32.png"),
            (64, "icon_32x32@2x.png"),
            (128, "icon_128x128.png"),
            (256, "icon_128x128@2x.png"),
            (256, "icon_256x256.png"),
            (512, "icon_256x256@2x.png"),
            (512, "icon_512x512.png"),
            (1024, "icon_512x512@2x.png"),
        ]
        for size, name in sizes:
            make_icon(size).save(os.path.join(iconset, name))
        try:
            subprocess.run(
                ["iconutil", "-c", "icns", iconset, "-o", icns_path],
                check=True, capture_output=True,
            )
            print(f"[ok] icon.icns (proper macOS icns, {os.path.getsize(icns_path)} bytes)")
        except subprocess.CalledProcessError as e:
            print(f"[warn] iconutil failed: {e.stderr.decode(errors='ignore')}")
            make_icon(512).save(icns_path)
            print(f"[warn] icon.icns is PNG fallback")
else:
    make_icon(512).save(icns_path)
    print(f"[warn] icon.icns is PNG placeholder (macOS-only iconutil used for real icns)")

print(f"\nAll icons generated -> {OUT}")
