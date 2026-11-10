"""Authors a small DVD-Video VIDEO_TS folder for testing Looker's ripper.

Two VOB IDs (each encoded separately, so timestamps restart between them), each split into two cells at a NAV
pack. Title 1: one PGC, four cells, three chapters. Title 2: its own PGC, the second VOB ID only. Streams: MPEG-2
720x576 interlaced, AC-3 5.1 English (0x80), LPCM stereo French (0xA0), one subpicture stream (0x20, declared
only). NAV packs get PCI/DSI filled in (lbn, vobu_ea, s/e ptm, next vobu, vob/cell ids) as dvdauthor would.

usage: python scripts/make_test_dvd.py <out dir>   (needs ffmpeg on PATH; native/PLAN.md N8)
"""
import os, struct, subprocess, sys

S = 2048
out = sys.argv[1]
vts_dir = os.path.join(out, "VIDEO_TS")
os.makedirs(vts_dir, exist_ok=True)
tmp = os.path.join(out, "_seg")
os.makedirs(tmp, exist_ok=True)

def encode(name, src, seconds, freq):
    path = os.path.join(tmp, name)
    subprocess.run([
        "ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", f"{src}=size=720x576:rate=25:duration={seconds}",
        "-f", "lavfi", "-i", f"sine=frequency={freq}:sample_rate=48000:duration={seconds}",
        "-f", "lavfi", "-i", f"sine=frequency={freq * 1.5}:sample_rate=48000:duration={seconds}",
        "-map", "0:v", "-map", "1:a", "-map", "2:a",
        "-target", "pal-dvd", "-aspect", "16:9", "-flags", "+ildct+ilme+cgop", "-field_order", "tt", "-g", "12", "-bf", "2", "-sc_threshold", "1000000000",
        "-c:a:0", "ac3", "-ac:a:0", "6", "-b:a:0", "384k",
        "-c:a:1", "pcm_dvd", "-ac:a:1", "2",
        path], check=True)
    return open(path, "rb").read()

segs = [encode("a.vob", "testsrc2", 20, 440), encode("b.vob", "smptebars", 14, 330)]

def navs(data):
    """Sector numbers of the NAV packs (pack header, system header, PCI)."""
    r = []
    for i in range(len(data) // S):
        s = data[i * S:(i + 1) * S]
        if s[:4] == b"\0\0\1\xba" and s[0x0E:0x12] == b"\0\0\1\xbb" and s[0x26:0x2A] == b"\0\0\1\xbf":
            r.append(i)
    return r

def video_ptss(data, a, b):
    out = []
    for i in range(a, b):
        s = data[i * S:(i + 1) * S]
        if s[:4] == b"\0\0\1\xba" and s[0x0E:0x12] == b"\0\0\1\xe0" and s[0x15] & 0x80:
            p = s[0x17:0x1C]
            pts = ((p[0] >> 1) & 7) << 30 | p[1] << 22 | (p[2] >> 1) << 15 | p[3] << 7 | p[4] >> 1
            out.append(pts)
    return out

vob = bytearray()
cells = []    # (vob_id, cell_id, first, last, first_pts, end_pts)
vobus = []    # all vobu starts
for vi, seg in enumerate(segs):
    base = len(vob) // S
    n = navs(seg)
    assert n and n[0] == 0, f"segment {vi} doesn't start with a NAV pack: {n[:3]}"
    total = len(seg) // S
    bounds = n + [total]
    split = n[len(n) // 2]
    seg = bytearray(seg)
    # Per-VOBU times.
    times = []
    for k in range(len(n)):
        p = video_ptss(seg, bounds[k], bounds[k + 1])
        times.append((min(p), max(p) + 3600))
    for k, s0 in enumerate(n):
        s1 = bounds[k + 1]
        cell_id = 1 if s0 < split else 2
        cell_end = (split if s0 < split else total)
        last_in_cell = s1 >= cell_end
        sec = bytearray(seg[s0 * S:(s0 + 1) * S])
        lbn = base + s0
        pci = 0x2D
        struct.pack_into(">I", sec, pci + 0, lbn)
        struct.pack_into(">I", sec, pci + 12, times[k][0])
        struct.pack_into(">I", sec, pci + 16, times[k][1])
        dsi = 0x407
        struct.pack_into(">I", sec, dsi + 0, times[k][0])  # nv_pck_scr (approx)
        struct.pack_into(">I", sec, dsi + 4, lbn)
        struct.pack_into(">I", sec, dsi + 8, s1 - s0 - 1)  # vobu_ea
        struct.pack_into(">H", sec, dsi + 24, vi + 1)       # vobu_vob_idn
        sec[dsi + 27] = cell_id                            # vobu_c_idn
        # vobu_sri: next_vobu at dsi+314 (0x3fffffff = end of cell), prev at 318.
        struct.pack_into(">I", sec, dsi + 234, 0x3FFFFFFF if last_in_cell else 0x80000000 | (s1 - s0))
        struct.pack_into(">I", sec, dsi + 314, 0x3FFFFFFF if last_in_cell else 0x80000000 | (s1 - s0))
        prev = 0x3FFFFFFF if s0 in (0, split) else 0x80000000 | (s0 - bounds[k - 1])
        struct.pack_into(">I", sec, dsi + 318, prev)
        seg[s0 * S:(s0 + 1) * S] = sec
        vobus.append(lbn)
    k_split = n.index(split)
    cells.append((vi + 1, 1, base, base + split - 1, times[0][0], times[k_split - 1][1]))
    cells.append((vi + 1, 2, base + split, base + total - 1, times[k_split][0], times[-1][1]))
    vob += seg

def bcd(ms):
    frames = int(round(ms % 1000 / 40))
    s = ms // 1000
    b = lambda v: ((v // 10) << 4) | (v % 10)
    return bytes([b(s // 3600), b(s // 60 % 60), b(s % 60), 0x40 | b(min(frames, 24))])

cell_ms = [(c[5] - c[4]) // 90 for c in cells]
print("cells", [(c[0], c[1], c[2], c[3]) for c in cells], cell_ms)

def pgc(cell_idx, programs):
    """A PGC playing cells[cell_idx] (in order), programs = entry cell numbers (1-based within this PGC)."""
    n = len(cell_idx)
    p = bytearray(0xEC)
    p[2] = len(programs)
    p[3] = n
    p[4:8] = bcd(sum(cell_ms[i] for i in cell_idx))
    struct.pack_into(">H", p, 0x0C, 0x8000 | (0 << 8))   # audio 0 -> stream 0 (0x80)
    struct.pack_into(">H", p, 0x0E, 0x8000 | (0 << 8))   # audio 1 -> stream 0 (0xA0)
    struct.pack_into(">I", p, 0x1C, 0x80000000)          # subpicture 0 -> stream 0 everywhere
    pal = [0x108080, 0xEB8080, 0x808080, 0x2D8080] + [0x108080] * 12
    for i, v in enumerate(pal):
        struct.pack_into(">I", p, 0xA4 + i * 4, v)
    prog_at = len(p)
    p += bytes(programs)
    if len(p) % 2:
        p += b"\0"
    play_at = len(p)
    for i in cell_idx:
        c = cells[i]
        e = bytearray(24)
        e[0] = 0
        e[4:8] = bcd(cell_ms[i])
        struct.pack_into(">IIII", e, 8, c[2], 0, c[3], c[3])
        # last_vobu_start_sector: the last VOBU's start
        last_vobu = max(v for v in vobus if v <= c[3])
        struct.pack_into(">I", e, 16, last_vobu)
        p += e
    pos_at = len(p)
    for i in cell_idx:
        p += struct.pack(">HBB", cells[i][0], 0, cells[i][1])
    struct.pack_into(">HHH", p, 0xE6, prog_at, play_at, pos_at)
    return bytes(p)

def sector_pad(b):
    return b + bytes((-len(b)) % S)

vob_sectors = len(vob) // S
open(os.path.join(vts_dir, "VTS_01_1.VOB"), "wb").write(vob)

# --- VTS_01_0.IFO ---
pgcs = [pgc([0, 1, 2, 3], [1, 2, 4]), pgc([2, 3], [1])]
ptt = bytearray(struct.pack(">HHI", 2, 0, 0))
ptt += struct.pack(">II", 16, 16 + 12)
ptt += struct.pack(">HH", 1, 1) + struct.pack(">HH", 1, 2) + struct.pack(">HH", 1, 3)
ptt += struct.pack(">HH", 2, 1)
struct.pack_into(">I", ptt, 4, len(ptt) - 1)
pgcit = bytearray(struct.pack(">HHI", len(pgcs), 0, 0))
off = 8 + 8 * len(pgcs)
body = b""
for i, p in enumerate(pgcs):
    pgcit += struct.pack(">BBHI", 0x80 | (i + 1), 0, 0, off + len(body))
    body += p
pgcit += body
struct.pack_into(">I", pgcit, 4, len(pgcit) - 1)
cadt = bytearray(struct.pack(">HHI", 2, 0, 0))
for c in cells:
    cadt += struct.pack(">HBBII", c[0], c[1], 0, c[2], c[3])
struct.pack_into(">I", cadt, 4, len(cadt) - 1)
admap = bytearray(struct.pack(">I", 0))
for v in vobus:
    admap += struct.pack(">I", v)
struct.pack_into(">I", admap, 0, len(admap) - 1)

tables = [sector_pad(bytes(ptt)), sector_pad(bytes(pgcit)), sector_pad(bytes(cadt)), sector_pad(bytes(admap))]
mat = bytearray(S)
mat[0:12] = b"DVDVIDEO-VTS"
ifo_sectors = 1 + sum(len(t) // S for t in tables)
struct.pack_into(">I", mat, 0x0C, ifo_sectors * 2 + vob_sectors - 1)
struct.pack_into(">I", mat, 0x1C, ifo_sectors - 1)
mat[0x21] = 0x11
struct.pack_into(">I", mat, 0x80, 0x3FF)
struct.pack_into(">I", mat, 0xC4, ifo_sectors)
at = 1
offs = []
for t in tables:
    offs.append(at)
    at += len(t) // S
struct.pack_into(">I", mat, 0xC8, offs[0])
struct.pack_into(">I", mat, 0xCC, offs[1])
struct.pack_into(">I", mat, 0xE0, offs[2])
struct.pack_into(">I", mat, 0xE4, offs[3])
# Video: MPEG-2, PAL, 16:9, 720x576.  mpeg_version(2) format(2) dar(2) df(2) | cc cc ? br size(2) lb film
struct.pack_into(">H", mat, 0x200, (1 << 14) | (1 << 12) | (3 << 10) | (1 << 8))
struct.pack_into(">H", mat, 0x202, 2)
# Audio 0: AC-3, lang, 6 ch, "en".  format(3) mc(1) langtype(2) app(2) | quant(2) freq(2) ? ch(3)
mat[0x204:0x20C] = bytes([(0 << 5) | (1 << 2), 5, ord("e"), ord("n"), 0, 1, 0, 0])
mat[0x20C:0x214] = bytes([(4 << 5) | (1 << 2), 1, ord("f"), ord("r"), 0, 1, 0, 0])
struct.pack_into(">H", mat, 0x254, 1)
mat[0x256:0x25C] = bytes([1, 0, ord("e"), ord("n"), 0, 1])
ifo = bytes(mat) + b"".join(tables)
for ext in ("IFO", "BUP"):
    open(os.path.join(vts_dir, f"VTS_01_0.{ext}"), "wb").write(ifo)

# --- VIDEO_TS.IFO ---
tt = bytearray(struct.pack(">HHI", 2, 0, 0))
tt += struct.pack(">BBHHBBI", 0x3C, 1, 3, 0, 1, 1, 0)
tt += struct.pack(">BBHHBBI", 0x3C, 1, 1, 0, 1, 2, 0)
struct.pack_into(">I", tt, 4, len(tt) - 1)
attr = bytearray(0x308)
struct.pack_into(">I", attr, 0, len(attr) - 1)
atrt = bytearray(struct.pack(">HHI", 1, 0, 0) + struct.pack(">I", 12)) + attr
struct.pack_into(">I", atrt, 4, len(atrt) - 1)
vmat = bytearray(S)
vmat[0:12] = b"DVDVIDEO-VMG"
vmg_tables = [sector_pad(bytes(tt)), sector_pad(bytes(atrt))]
vmg_sectors = 1 + sum(len(t) // S for t in vmg_tables)
struct.pack_into(">I", vmat, 0x0C, vmg_sectors * 2 - 1)
struct.pack_into(">I", vmat, 0x1C, vmg_sectors - 1)
vmat[0x21] = 0x11
struct.pack_into(">HH", vmat, 0x26, 1, 1)
vmat[0x2A] = 1
struct.pack_into(">H", vmat, 0x3E, 1)
vmat[0x40:0x40 + 11] = b"LOOKER-TEST"
struct.pack_into(">I", vmat, 0x80, 0x3FF)
struct.pack_into(">I", vmat, 0xC4, 1)
# First Play PGC at 0x400: no cells, one pre-command, JumpTT 1.
fp = bytearray(0xEC)
struct.pack_into(">H", fp, 0xE4, 0xEC)
fp += struct.pack(">HHHH", 1, 0, 0, 8 + 8 - 1) + bytes([0x30, 0x02, 0, 0, 0, 1, 0, 0])
vmat[0x400:0x400 + len(fp)] = fp
struct.pack_into(">I", vmat, 0x84, 0x400)
struct.pack_into(">I", vmat, 0xD0, 1 + len(vmg_tables[0]) // S)
vmg = bytes(vmat) + b"".join(vmg_tables)
for ext in ("IFO", "BUP"):
    open(os.path.join(vts_dir, f"VIDEO_TS.{ext}"), "wb").write(vmg)
for f in os.listdir(tmp):
    os.remove(os.path.join(tmp, f))
os.rmdir(tmp)
print("wrote", vts_dir, vob_sectors, "sectors,", len(vobus), "VOBUs")
