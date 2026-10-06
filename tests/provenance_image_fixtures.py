"""Synthetic image inputs only; no candidate output or private files.

ICC gamma profiles retain the old Pillow-generated sRGB matrix and replace only
TRCs with a hand-written ICC curv tag (gamma1.5). TIFF cases are tiny scalar arrays.
"""
from pathlib import Path
import io
import struct
import zlib
from PIL import Image, ImageCms


def srgb_profile():
    return ImageCms.ImageCmsProfile(ImageCms.createProfile('sRGB')).tobytes()


def gamma_profile(padded=False):
    original = srgb_profile()
    count = struct.unpack_from('>I', original, 128)[0]
    tags = []
    for i in range(count):
        signature, offset, length = struct.unpack_from('>4sII', original, 132 + i * 12)
        payload = original[offset:offset + length]
        if signature in (b'rTRC', b'gTRC', b'bTRC'):
            payload = b'curv' + b'\0' * 4 + struct.pack('>IH', 1, 384)  # gamma 1.5 in u8Fixed8
        tags.append((signature, payload))
    offset = 132 + len(tags) * 12
    entries = bytearray()
    payloads = bytearray()
    for signature, payload in tags:
        entries.extend(struct.pack('>4sII', signature, offset + len(payloads), len(payload)))
        payloads.extend(payload)
        payloads.extend(b'\0' * (-len(payload) % 4))
    profile = bytearray(original[:128] + struct.pack('>I', len(tags)) + entries + payloads)
    if padded:
        profile.extend(b'\0' * 70000)  # Forces multiple JPEG APP2 fragments, no external profile.
    struct.pack_into('>I', profile, 0, len(profile))
    profile[84:100] = b'\0' * 16  # Unspecified profile ID after rebuilding the tag table.
    return bytes(profile)


def rgb_image(mode='RGB'):
    rgb = Image.new('RGB', (23, 17))
    rgb.putdata([(x * 13 % 256, x * 47 % 256, x * 59 % 256) for x in range(23 * 17)])
    image = rgb.convert(mode)
    if mode in ('RGBA', 'LA'):
        image.putalpha(Image.frombytes('L', image.size, bytes(x * 23 % 256 for x in range(23 * 17))))
    return image


def jpeg_icc_variant(data, variant):
    """Perturb only APP2 fragment order/count, using a synthetic JPEG input."""
    with Image.open(io.BytesIO(data)) as image:
        parts = [payload for tag, payload in image.applist
                 if tag == 'APP2' and payload.startswith(b'ICC_PROFILE\0')]
    packets = [b'\xff\xe2' + struct.pack('>H', len(part) + 2) + part for part in parts]
    offset = data.index(packets[0])
    for packet in packets:
        data = data.replace(packet, b'', 1)
    if variant == 'reverse':
        parts.reverse()
    elif variant == 'bad-total':
        parts = [part[:13] + bytes([len(parts) + 1]) + part[14:] for part in parts]
    elif variant == 'missing':
        parts.pop()
    elif variant == 'zero-first-sequence':
        parts[0] = parts[0][:12] + b'\0' + parts[0][13:]
    else:
        raise ValueError(variant)
    packets = [b'\xff\xe2' + struct.pack('>H', len(part) + 2) + part for part in parts]
    return data[:offset] + b''.join(packets) + data[offset:]


def scalar_image(mode):
    payload = {
        'I;16': struct.pack('<6H', 0, 1, 127, 255, 256, 65535),
        'I;16B': struct.pack('>6H', 0, 1, 127, 255, 256, 65535),
        'I': struct.pack('<6i', -2147483648, -1, 0, 1, 255, 2147483647),
        'F': struct.pack('<6f', float('nan'), -float('inf'), float('inf'), -.25, .75, 127.75),
    }[mode]
    return Image.frombytes(mode, (3, 2), payload)


def scalar_tiffs(root):
    for mode in ('I;16', 'I;16B', 'I', 'F'):
        for compression in ('raw', 'tiff_adobe_deflate'):
            for orientation in range(1, 9):
                path = Path(root) / f'{mode}-{compression}-{orientation}.tiff'
                scalar_image(mode).save(path, compression=compression, tiffinfo={274: orientation})
                yield mode, compression, orientation, path


def raw_u16_tiff(*, endian='<', width=3, height=2, rows=None,
                 orientation=1, fill_order=1, values=None, compression=1):
    return scalar_tiff(endian=endian, width=width, height=height, rows=rows,
                       orientation=orientation, fill_order=fill_order,
                       values=values, compression=compression)


def scalar_tiff(*, mode='u16', endian='<', width=3, height=2, rows=None,
                orientation=1, fill_order=1, values=None, compression=1,
                icc=None, planar=1, predictor=1):
    """Hand-written BlackIsZero TIFF strips, independent of either decoder.

    Values describe stored words; FillOrder is deliberately not applied here.
    Deflate8/32946 contain identical zlib streams; their tags remain distinct.
    """
    rows = height if rows is None else rows
    kind, bits, sample, defaults = {
        'u16': ('H', 16, 1, [0, 1, 127, 255, 256, 65535]),
        'i32': ('i', 32, 2, [-2147483648, -1, 0, 127, 256, 2147483647]),
        'f32': ('f', 32, 3, [float('nan'), -float('inf'), float('inf'), -.25, .75, 127.75]),
    }[mode]
    values = defaults if values is None else values
    if len(values) != width * height or rows < 1:
        raise ValueError('invalid synthetic dimensions')
    strips = [struct.pack(endian + f'{len(values[y * width:min(y + rows, height) * width])}{kind}',
                          *values[y * width:min(y + rows, height) * width])
              for y in range(0, height, rows)]
    if compression in (8, 32946):
        strips = [zlib.compress(strip) for strip in strips]
        if fill_order == 2:
            strips = [bytes(int(f'{v:08b}'[::-1], 2) for v in strip) for strip in strips]
    elif compression != 1:
        raise ValueError(compression)
    tags = {256: (4, [width]), 257: (4, [height]), 258: (3, [bits]),
            259: (3, [compression]), 262: (3, [1]), 266: (3, [fill_order]),
            273: (4, [0] * len(strips)), 274: (3, [orientation]),
            277: (3, [1]), 278: (4, [rows]),
            279: (4, [len(strip) for strip in strips]), 284: (3, [planar]),
            317: (3, [predictor]), 339: (3, [sample])}
    if icc is not None:
        tags[34675] = (7, icc)
    def encode(kind, values):
        return bytes(values) if kind == 7 else struct.pack(endian + ('H' if kind == 3 else 'I') * len(values), *values)
    extra_start = 8 + 2 + len(tags) * 12 + 4
    data_start = extra_start + sum((len(data) + 1) // 2 * 2 for kind, values in tags.values()
                                   if len(data := encode(kind, values)) > 4)
    offsets = []
    for strip in strips:
        offsets.append(data_start)
        data_start += len(strip)
    tags[273] = (4, offsets)
    entries, extra = bytearray(), bytearray()
    for tag, (kind, values) in sorted(tags.items()):
        encoded = encode(kind, values)
        slot = (encoded.ljust(4, b'\0') if len(encoded) <= 4 else
                struct.pack(endian + 'I', extra_start + len(extra)))
        entries.extend(struct.pack(endian + 'HHI', tag, kind, len(values)) + slot)
        if len(encoded) > 4:
            extra.extend(encoded)
            extra.extend(b'\0' * (len(encoded) % 2))
    return ((b'II' if endian == '<' else b'MM') + struct.pack(endian + 'HIH', 42, 8, len(tags))
            + entries + b'\0' * 4 + extra + b''.join(strips))
