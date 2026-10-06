#!/usr/bin/env python3
"""
Erzeugt ein 256x256 PNG-Icon für ink-notes mit Python Standard-Bibliothek (ohne externe Module).
"""

import math
import os
import struct
import zlib

WIDTH = 256
HEIGHT = 256


def make_png(width, height, rgba_data):
    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = b"\x89PNG\r\n\x1a\n"
    ihdr = chunk(tag=b"IHDR", data=struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))

    raw = bytearray()
    for y in range(height):
        raw.append(0)  # Filter type 0 (None)
        raw.extend(rgba_data[y * width * 4 : (y + 1) * width * 4])

    idat = chunk(tag=b"IDAT", data=zlib.compress(bytes(raw), 9))
    iend = chunk(tag=b"IEND", data=b"")
    return header + ihdr + idat + iend


def render_icon():
    buffer = bytearray(WIDTH * HEIGHT * 4)

    def set_pixel(x, y, r, g, b, a):
        if 0 <= x < WIDTH and 0 <= y < HEIGHT:
            idx = (y * WIDTH + x) * 4
            src_a = a / 255.0
            dst_a = buffer[idx + 3] / 255.0
            out_a = src_a + dst_a * (1.0 - src_a)
            if out_a > 1e-5:
                out_r = (r * src_a + buffer[idx] * dst_a * (1.0 - src_a)) / out_a
                out_g = (
                    g * src_a + buffer[idx + 1] * dst_a * (1.0 - src_a)
                ) / out_a
                out_b = (
                    b * src_a + buffer[idx + 2] * dst_a * (1.0 - src_a)
                ) / out_a
                buffer[idx] = int(round(out_r))
                buffer[idx + 1] = int(round(out_g))
                buffer[idx + 2] = int(round(out_b))
                buffer[idx + 3] = int(round(out_a * 255.0))

    # 1. Abgerundete Squircle-Kachel (Zorin OS Stil)
    cx, cy = WIDTH / 2.0, HEIGHT / 2.0
    half_size = 114.0
    radius = 52.0

    for y in range(HEIGHT):
        for x in range(WIDTH):
            dx = abs(x - cx)
            dy = abs(y - cy)
            # Distance from rounded box
            qx = max(0.0, dx - (half_size - radius))
            qy = max(0.0, dy - (half_size - radius))
            dist = math.sqrt(qx * qx + qy * qy)

            if dx <= half_size and dy <= half_size and dist <= radius:
                # Gradient von Dunkelblau (#1e2638) nach Nachtblau (#0f1422)
                t = (x + y) / (WIDTH + HEIGHT)
                r = int(30 * (1 - t) + 12 * t)
                g = int(38 * (1 - t) + 16 * t)
                b = int(58 * (1 - t) + 28 * t)

                # Rand-Antialiasing
                edge_dist = radius - dist
                alpha = min(255, max(0, int(edge_dist * 255)))

                # Subtiler Rahmen
                if edge_dist < 2.5:
                    r, g, b = 60, 75, 110

                set_pixel(x, y, r, g, b, alpha)

    # 2. Subtile Gitterlinien
    for gy in range(80, 200, 30):
        for gx in range(40, 216):
            set_pixel(gx, gy, 140, 160, 190, 25)
    for gx in range(70, 200, 30):
        for gy in range(50, 216):
            set_pixel(gx, gy, 140, 160, 190, 25)

    # 3. Tintenspur (Bézier-Kurve mit Glow)
    pts = []
    for t_i in range(150):
        t = t_i / 150.0
        # P0=(55, 195), P1=(90, 215), P2=(130, 190), P3=(120, 155), P4=(140, 110), P5=(170, 110)
        # 2 Teilkurven
        if t <= 0.5:
            u = t * 2.0
            bx = (
                (1 - u) ** 3 * 55
                + 3 * (1 - u) ** 2 * u * 90
                + 3 * (1 - u) * u**2 * 130
                + u**3 * 120
            )
            by = (
                (1 - u) ** 3 * 195
                + 3 * (1 - u) ** 2 * u * 215
                + 3 * (1 - u) * u**2 * 190
                + u**3 * 155
            )
        else:
            u = (t - 0.5) * 2.0
            bx = (
                (1 - u) ** 3 * 120
                + 3 * (1 - u) ** 2 * u * 110
                + 3 * (1 - u) * u**2 * 140
                + u**3 * 170
            )
            by = (
                (1 - u) ** 3 * 155
                + 3 * (1 - u) ** 2 * u * 120
                + 3 * (1 - u) * u**2 * 95
                + u**3 * 110
            )
        pts.append((bx, by))

    for px, py in pts:
        for ox in range(-7, 8):
            for oy in range(-7, 8):
                d = math.sqrt(ox * ox + oy * oy)
                if d <= 7.0:
                    a = int((1.0 - d / 7.0) * 110)
                    set_pixel(int(px + ox), int(py + oy), 59, 130, 246, a)
        for ox in range(-3, 4):
            for oy in range(-3, 4):
                d = math.sqrt(ox * ox + oy * oy)
                if d <= 3.0:
                    a = int((1.0 - d / 3.0) * 230)
                    set_pixel(int(px + ox), int(py + oy), 147, 197, 253, a)

    # 4. Füllfederhalter (diagonale Geometrie)
    # Stift-Mittelachse von (120, 55) nach (170, 110)
    pen_dir = (math.cos(math.radians(45)), math.sin(math.radians(45)))
    norm = (-pen_dir[1], pen_dir[0])

    # Spitze bei (110, 195)
    tip_x, tip_y = 55.0, 195.0
    for l in range(0, 120):
        pos_x = tip_x + pen_dir[0] * l
        pos_y = tip_y - pen_dir[1] * l
        # Radius entlang des Stifts
        if l < 20:
            # Goldene Feder
            w = (l / 20.0) * 10.0
            color = (245, 158, 11)  # Gold
        elif l < 32:
            # Griffzone
            w = 11.0
            color = (30, 41, 59)  # Dunkelgrau
        else:
            # Schaft
            w = 12.0
            color = (59, 130, 246)  # Blau

        for s in range(int(-w), int(w) + 1):
            px = int(pos_x + norm[0] * s)
            py = int(pos_y + norm[1] * s)
            set_pixel(px, py, color[0], color[1], color[2], 255)

    # 5. Glanzpunkt auf der Spitze
    for ox in range(-4, 5):
        for oy in range(-4, 5):
            d = math.sqrt(ox * ox + oy * oy)
            if d <= 4:
                set_pixel(int(tip_x + ox), int(tip_y + oy), 255, 255, 255, 255)

    return make_png(WIDTH, HEIGHT, buffer)


if __name__ == "__main__":
    os.makedirs("assets", exist_ok=True)
    png_data = render_icon()
    with open("assets/ink-notes.png", "wb") as f:
        f.write(png_data)
    print("PNG-Icon erfolgreich generiert: assets/ink-notes.png (256x256)")
