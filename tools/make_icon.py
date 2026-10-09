# -*- coding: utf-8 -*-
"""生成 assets/icon.ico（托盘图标）。

只依赖标准库：手工拼 BMP 格式的 32bpp ICO，用 4 倍超采样做抗锯齿。
图案是一个琥珀色圆角方块 + 三条深色横条，读起来像"候选框"。

    python tools/make_icon.py
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

SIZES = (16, 24, 32, 48)
SS = 4  # 超采样倍数

BG = (0xC7, 0x9A, 0x3A)   # 琥珀色底
BAR = (0x1A, 0x20, 0x29)  # 深色横条

# 相对尺寸：(x 起点, y 起点, 宽度, 高度)，单位是画布比例
LAYOUT = (
    (0.22, 0.21, 0.56, 0.11),
    (0.22, 0.42, 0.44, 0.11),
    (0.22, 0.63, 0.30, 0.11),
)


def inside_round_rect(x: float, y: float, w: float, h: float, radius: float) -> bool:
    """点 (x, y) 是否落在圆角矩形内（圆角矩形的 SDF）。"""
    dx = abs(x - w / 2) - (w / 2 - radius)
    dy = abs(y - h / 2) - (h / 2 - radius)
    dx = max(dx, 0.0)
    dy = max(dy, 0.0)
    return dx * dx + dy * dy <= radius * radius


def sample(x: float, y: float, size: float) -> tuple[float, float, float, float]:
    """返回该点的非预乘 (r, g, b, a)，分量范围 0..1。"""
    radius = size * 0.26
    if not inside_round_rect(x, y, size, size, radius):
        return (0.0, 0.0, 0.0, 0.0)

    for bx, by, bw, bh in LAYOUT:
        x0, y0 = bx * size, by * size
        w, h = bw * size, bh * size
        r = min(w, h) / 2
        if inside_round_rect(x - x0, y - y0, w, h, r):
            return (*[c / 255 for c in BAR], 1.0)

    return (*[c / 255 for c in BG], 1.0)


def render(size: int) -> list[tuple[int, int, int, int]]:
    """渲染一张 size×size 的 BGRA 像素表，行序自下而上（BMP 约定）。"""
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            # 超采样：在输出像素内均匀取 SS×SS 个点
            sa = sr = sg = sb = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    x = px + (sx + 0.5) / SS
                    y = py + (sy + 0.5) / SS
                    r, g, b, a = sample(x, y, float(size))
                    # 在预乘空间里平均，避免边缘出现黑边
                    sa += a
                    sr += r * a
                    sg += g * a
                    sb += b * a
            n = SS * SS
            a = sa / n
            if a > 0.0:
                r, g, b = sr / sa, sg / sa, sb / sa
            else:
                r = g = b = 0.0
            row.append(
                (
                    int(round(b * 255)),
                    int(round(g * 255)),
                    int(round(r * 255)),
                    int(round(a * 255)),
                )
            )
        rows.append(row)
    rows.reverse()  # BMP 是自下而上存的
    return rows


def bmp_image(size: int) -> bytes:
    pixels = render(size)
    xor = bytearray()
    for row in pixels:
        for b, g, r, a in row:
            xor += bytes((b, g, r, a))
    # 32bpp 图标仍需一张 AND 掩码，全 0 表示完全交给 alpha 通道
    mask_stride = ((size + 31) // 32) * 4
    mask = bytes(mask_stride * size)
    header = struct.pack(
        "<IiiHHIIiiII",
        40,          # biSize
        size,        # biWidth
        size * 2,    # biHeight（XOR + AND 两张图的高度）
        1,           # biPlanes
        32,          # biBitCount
        0,           # biCompression = BI_RGB
        len(xor) + len(mask),
        0, 0, 0, 0,
    )
    return header + bytes(xor) + mask


def build_ico() -> bytes:
    images = [(size, bmp_image(size)) for size in SIZES]
    directory = struct.pack("<HHH", 0, 1, len(images))
    entries = bytearray()
    offset = 6 + 16 * len(images)
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII",
            size if size < 256 else 0,
            size if size < 256 else 0,
            0,      # 调色板色数
            0,      # 保留
            1,      # 色彩平面
            32,     # 位深
            len(data),
            offset,
        )
        offset += len(data)
    return directory + bytes(entries) + b"".join(data for _, data in images)


def main() -> int:
    target = Path(__file__).resolve().parent.parent / "assets" / "icon.ico"
    target.parent.mkdir(parents=True, exist_ok=True)
    data = build_ico()
    target.write_bytes(data)
    print("已写入 %s（%d 字节，%s）" % (target, len(data), "/".join(map(str, SIZES))))
    return 0


if __name__ == "__main__":
    sys.exit(main())
