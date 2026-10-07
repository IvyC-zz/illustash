"""#152 independent legacy NumPy/Pillow reference and handwritten invariants.

Only synthetic images/isolated SQLite snapshots. Explicit candidate binaries;
no build, real model, library, Photos or process discovery in these tests.
"""
import json
import io
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import tempfile
import unittest
import zlib

import numpy as np
from PIL import Image, ImageDraw

from web import relate, similar
from tests.relate_images import picture, jpeg


class NativeRelationImagesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_RELATION_PROBE_BIN')
        if not configured:
            raise unittest.SkipTest('explicit candidate relation probe required')
        cls.binary = Path(configured).resolve()
        if not cls.binary.is_file():
            raise AssertionError('configured candidate relation probe missing')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def probe(self, op, **args):
        request, response = self.root / 'probe.json', self.root / 'out.json'
        request.write_text(json.dumps(dict(op=op, **args)), encoding='utf-8')
        result = subprocess.run([str(self.binary), str(request), str(response)],
                                env=os.environ | {'PATH': ''}, capture_output=True, timeout=180)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        return json.loads(response.read_text())

    def save(self, im, name='test.png', **args):
        path = self.root / name
        im.save(path, **args)
        return str(path)

    def raw_png(self, pixels, colour, depth, name, *, transparency=None):
        # Independent encoder: no candidate codec is used to make the fixture.
        height,width,channels=pixels.shape
        rows=[]
        for row in pixels:
            values=row.ravel().tolist()
            if depth==16:
                data=b''.join(struct.pack('>H',int(v)) for v in values)
            elif depth==8:
                data=bytes(values)
            else:
                data=bytearray((len(values)*depth+7)//8)
                for i,v in enumerate(values):data[i*depth//8]|=int(v)<<(8-depth-i*depth%8)
                data=bytes(data)
            rows.append(b'\0'+data)
        def chunk(kind,data):
            return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data))
        raw=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',width,height,depth,colour,0,0,0))
        if colour==3:
            raw+=chunk(b'PLTE',bytes(v for i in range(1<<depth) for v in ((i*71)%256,(i*137)%256,(i*193)%256)))
        if transparency is not None:raw+=chunk(b'tRNS',transparency)
        raw+=chunk(b'IDAT',zlib.compress(b''.join(rows)))+chunk(b'IEND',b'')
        path=self.root/name;path.write_bytes(raw)
        return str(path)

    def raw_rgb16_tiff(self,pixels,name,*,big_endian=False):
        # Handwritten classic TIFF: unsigned chunky RGB(A), one uncompressed strip.
        height,width,channels=pixels.shape
        endian='>' if big_endian else '<'
        tags=[(256,4,1,width),(257,4,1,height),(258,3,channels,0),(259,3,1,1),
              (262,3,1,2),(273,4,1,0),(277,3,1,channels),(278,4,1,height),
              (279,4,1,pixels.size*2),(284,3,1,1)]
        if channels==4:tags.append((338,3,1,2))
        tags.sort()
        bits_offset=8+2+len(tags)*12+4
        pixel_offset=bits_offset+channels*2
        raw=(b'MM' if big_endian else b'II')+struct.pack(endian+'HIH',42,8,len(tags))
        for tag,kind,count,value in tags:
            if tag==258:value=bits_offset
            if tag==273:value=pixel_offset
            field=struct.pack(endian+'H',value)+b'\0\0' if kind==3 and count==1 else struct.pack(endian+'I',value)
            raw+=struct.pack(endian+'HHI',tag,kind,count)+field
        raw+=struct.pack(endian+'I',0)+struct.pack(endian+'H'*channels,*([16]*channels))+pixels.astype(endian+'u2').tobytes()
        path=self.root/name;path.write_bytes(raw)
        return str(path)

    def raw_byte_tiff(self,pixels,name,*,big_endian=False,planar=False,associated=False,deflate=False,rows=7,tiled=False):
        # Independent encoder for raw channel order/alpha. No candidate encoder.
        height,width,bands=pixels.shape
        bits=pixels.dtype.itemsize*8
        endian='>' if big_endian else '<'
        encode=lambda data:data.astype(endian+'u'+str(pixels.dtype.itemsize)).tobytes()
        blocks=[]
        for plane in range(bands if planar else 1):
            if tiled:
                for y in range(0,height,16):
                    for x in range(0,width,16):
                        shape=(16,16) if planar else (16,16,bands)
                        tile=np.zeros(shape,pixels.dtype)
                        src=pixels[y:y+16,x:x+16,plane] if planar else pixels[y:y+16,x:x+16]
                        tile[:src.shape[0],:src.shape[1]]=src
                        data=encode(tile)
                        blocks.append(zlib.compress(data) if deflate else data)
            else:
                for y in range(0,height,rows):
                    data=encode(pixels[y:y+rows,:,plane] if planar else pixels[y:y+rows])
                    blocks.append(zlib.compress(data) if deflate else data)
        short=lambda n:struct.pack(endian+'H',n)
        long=lambda n:struct.pack(endian+'I',n)
        fields={256:(4,1,long(width)),257:(4,1,long(height)),258:(3,bands,short(bits)*bands),
                259:(3,1,short(8 if deflate else 1)),262:(3,1,short(2)),273:(4,len(blocks),bytes(4*len(blocks))),
                277:(3,1,short(bands)),278:(4,1,long(rows)),279:(4,len(blocks),b''.join(long(len(b)) for b in blocks)),
                284:(3,1,short(2 if planar else 1))}
        if bands==4:fields[338]=(3,1,short(1 if associated else 2))
        offset_tag=273
        if tiled:
            fields[324]=fields.pop(273);fields[325]=fields.pop(279);fields.pop(278)
            fields[322]=(4,1,long(16));fields[323]=(4,1,long(16));offset_tag=324
        external={};offset=8+2+len(fields)*12+4
        for tag,(_,_,data) in sorted(fields.items()):
            if len(data)>4:external[tag]=offset;offset+=(len(data)+1)//2*2
        offsets=[]
        for block in blocks:offsets.append(offset);offset+=len(block)
        fields[offset_tag]=(4,len(blocks),b''.join(long(n) for n in offsets))
        raw=(b'MM' if big_endian else b'II')+short(42)+long(8)+short(len(fields))
        extras=b''
        for tag,(kind,count,data) in sorted(fields.items()):
            raw+=short(tag)+short(kind)+long(count)+(long(external[tag]) if len(data)>4 else data.ljust(4,b'\0'))
            if len(data)>4:extras+=data+b'\0'*(len(data)%2)
        raw+=long(0)+extras+b''.join(blocks)
        path=self.root/name;path.write_bytes(raw);return str(path)

    def raw_scalar_tiff(self,pixels,name,*,big_endian=False,orientation=1,fill_order=1,rows=None,deflate=False,icc=b'synthetic ICC',planar=1,photo=1,bits=None,codec=None,palette=None):
        # Independent classic TIFF encoder: numeric scalar bytes, real strips and
        # optional zlib. The candidate is never used to encode an expected input.
        height,width=pixels.shape
        endian='>' if big_endian else '<'
        kind=pixels.dtype.kind;bits=bits or pixels.dtype.itemsize*8
        sample=3 if kind=='f' else (2 if kind=='i' else 1)
        rows=height if rows is None else rows
        if bits<8:
            packed=[]
            for row in pixels:
                data=bytearray((width*bits+7)//8)
                for x,v in enumerate(row):data[x*bits//8]|=int(v)<<(8-bits-x*bits%8)
                packed.append(bytes(data))
            blocks=[b''.join(packed[y:y+rows]) for y in range(0,height,rows)]
        elif bits==12:
            packed=[]
            for row in pixels:
                data=bytearray((width*12+7)//8)
                for x,v in enumerate(row):
                    bit=x*12;at=bit//8;n=int(v)<<(4-bit%8)
                    data[at]|=n>>8;data[at+1]|=n&255
                packed.append(bytes(data))
            blocks=[b''.join(packed[y:y+rows]) for y in range(0,height,rows)]
        else:
            blocks=[pixels[y:min(y+rows,height)].astype(endian+kind+str(pixels.dtype.itemsize)).tobytes()
                    for y in range(0,height,rows)]
        compression=8 if deflate else 1
        if codec=='packbits':
            blocks=[b''.join(bytes([len(block[i:i+128])-1])+block[i:i+128] for i in range(0,len(block),128)) for block in blocks]
            compression=32773
        elif codec=='lzw':
            encoded=[]
            for block in blocks:
                codes=[]
                for i in range(0,len(block),200):codes+=[256]+list(block[i:i+200])
                codes+=[257];data=bytearray((len(codes)*9+7)//8)
                for i,code in enumerate(codes):
                    for j in range(9):data[(i*9+j)//8]|=((code>>(8-j))&1)<<(7-(i*9+j)%8)
                encoded.append(bytes(data))
            blocks=encoded;compression=5
        elif deflate:blocks=[zlib.compress(block) for block in blocks]
        if fill_order==2 and compression!=1:
            blocks=[bytes(int(f'{b:08b}'[::-1],2) for b in block) for block in blocks]
        short=lambda value:struct.pack(endian+'H',value)
        long=lambda value:struct.pack(endian+'I',value)
        fields={256:(4,1,long(width)),257:(4,1,long(height)),258:(3,1,short(bits)),
                259:(3,1,short(compression)),262:(3,1,short(photo)),266:(3,1,short(fill_order)),
                273:(4,len(blocks),bytes(4*len(blocks))),274:(3,1,short(orientation)),277:(3,1,short(1)),
                278:(4,1,long(rows)),279:(4,len(blocks),b''.join(long(len(block)) for block in blocks)),284:(3,1,short(planar)),
                339:(3,1,short(sample))}
        if palette is not None:fields[320]=(3,len(palette),b''.join(short(v) for v in palette))
        if icc is not None:fields[34675]=(7,len(icc),icc)
        external={};offset=8+2+len(fields)*12+4
        for tag,(dtype,count,data) in sorted(fields.items()):
            if len(data)>4:
                external[tag]=offset;offset+=(len(data)+1)//2*2
        pixels_offset=offset
        offsets=[]
        for block in blocks:offsets.append(offset);offset+=len(block)
        fields[273]=(4,len(blocks),b''.join(long(offset) for offset in offsets))
        raw=(b'MM' if big_endian else b'II')+short(42)+long(8)+short(len(fields))
        extras=b''
        for tag,(dtype,count,data) in sorted(fields.items()):
            value=long(external[tag]) if len(data)>4 else data.ljust(4,b'\0')
            raw+=short(tag)+short(dtype)+long(count)+value
            if len(data)>4:extras+=data+b'\0'*(len(data)%2)
        raw+=long(0)+extras
        assert len(raw)==pixels_offset
        raw+=b''.join(blocks)
        path=self.root/name;path.write_bytes(raw);return str(path)

    def assert_image(self, path):
        expected, size = relate.load(path)
        actual = self.probe('load', path=path)
        self.assertEqual(actual['size'], list(size))
        self.assertEqual((actual['height'], actual['width']), expected.shape[:2])
        actual_bits = np.asarray(actual['bits'], np.uint32)
        np.testing.assert_array_equal(actual_bits, expected.view(np.uint32).ravel())
        ratio, grid = similar.signature(path)
        actual = self.probe('signature', path=path)
        self.assertEqual(actual['ratio'], ratio)
        self.assertEqual(bytes(actual['grid']), grid)
        ratio_bytes,grid_bytes=similar.signature(io.BytesIO(Path(path).read_bytes()))
        self.assertEqual(self.probe('signature_bytes', path=path),dict(ratio=ratio_bytes,grid=list(grid_bytes)))
        from provenance.verify import _colour
        expected_colour = _colour(path).astype(np.uint8).ravel().tolist()
        self.assertEqual(self.probe('colour', path=path), expected_colour)
        expected_bytes=_colour(io.BytesIO(Path(path).read_bytes())).astype(np.uint8).ravel().tolist()
        self.assertEqual(self.probe('colour_bytes', path=path), expected_bytes)

    def test_display_bytes_signatures_modes_transparency_orientation_and_draft(self):
        rng = np.random.default_rng(15)
        rgb = rng.integers(0, 256, (87, 59, 3), np.uint8)
        for mode in ['RGB', 'L', 'RGBA', 'LA', 'P', '1']:
            im = Image.fromarray(rgb).convert(mode)
            if mode == 'RGBA':
                im.putalpha(Image.fromarray(rng.integers(0, 256, (87, 59), np.uint8)))
            elif mode == 'P':
                im.info['transparency'] = 1
            with self.subTest(mode=mode):
                self.assert_image(self.save(im, mode + '.png'))
        gray16 = Image.fromarray(np.arange(87*59, dtype=np.uint16).reshape(87, 59))
        self.assert_image(self.save(gray16, 'gray16.png'))
        for fmt in ['PNG', 'TIFF', 'JPEG', 'WEBP', 'BMP', 'GIF', 'PPM']:
            im = Image.fromarray(rgb)
            exif = Image.Exif()
            exif[274] = 6
            with self.subTest(fmt=fmt):
                self.assert_image(self.save(im, 'rotated.' + fmt.lower(), format=fmt, exif=exif))
        big = picture(2, (1301, 1703))
        self.assert_image(self.save(big, 'large.png'))
        self.assert_image(self.save(big, 'large.jpg', quality=83))
        self.assert_image(self.save(big.convert('CMYK'), 'cmyk.jpg', quality=83))
        self.assert_image(self.save(big.convert('RGBA'), 'large-alpha.png'))

    def test_tiff_original_palette_packed_and_gray_alpha_modes(self):
        rng = np.random.default_rng(531)
        for size in [(59,87),(1057,1409)]:
            rgb = rng.integers(0,256,(size[1],size[0],3),np.uint8)
            for mode in ['P','1','LA']:
                image=Image.fromarray(rgb).convert(mode)
                if mode=='LA':
                    image.putalpha(Image.fromarray(rng.integers(0,256,(size[1],size[0]),np.uint8)))
                for compression in ['raw','tiff_deflate','packbits','tiff_lzw']:
                    orientations = range(1,9) if size==(59,87) and compression=='raw' else [1]
                    for orientation in orientations:
                        with self.subTest(size=size,mode=mode,compression=compression,orientation=orientation):
                            self.assert_image(self.save(image,'mode.tif',compression=compression,tiffinfo={274:orientation}))

    def test_tiff_planar_and_associated_alpha_original_display_and_colour(self):
        rng=np.random.default_rng(661)
        pixels=rng.integers(0,256,(31,29,4),np.uint8)
        pixels[::3,::2]=[50,30,10,0]
        for big in [False,True]:
            for planar in [False,True]:
                for associated in [False,True]:
                    for deflate in [False,True]:
                        with self.subTest(big=big,planar=planar,associated=associated,deflate=deflate):
                            path=self.raw_byte_tiff(pixels,'channels.tif',big_endian=big,
                                planar=planar,associated=associated,deflate=deflate)
                            if planar and associated and not deflate:
                                with self.assertRaisesRegex(ValueError,'unknown raw mode'):
                                    relate.load(path)
                                request,response=self.root/'invalid.json',self.root/'invalid-out.json'
                                request.write_text(json.dumps(dict(op='load',path=path)))
                                run=subprocess.run([str(self.binary),str(request),str(response)],
                                    env=os.environ|{'PATH':''},capture_output=True,timeout=30)
                                self.assertNotEqual(run.returncode,0)
                                self.assertIn('unknown raw mode',run.stderr.decode())
                                self.assertFalse(response.exists())
                            else:
                                self.assert_image(path)
        for big in [False,True]:
            self.assert_image(self.raw_byte_tiff(pixels[:,:,:3],'rgb-planar.tif',big_endian=big,planar=True))

    def test_tiff_planar_tile_bottom_and_right_padding(self):
        pixels=np.random.default_rng(812).integers(0,256,(19,17,4),np.uint8)
        for bands in [3,4]:
            for deflate in [False,True]:
                with self.subTest(bands=bands,deflate=deflate):
                    self.assert_image(self.raw_byte_tiff(pixels[:,:,:bands],'tiles.tif',planar=True,
                        tiled=True,deflate=deflate))

    def test_tiff_additional_signed16_unsigned32_and_white_zero_scalars(self):
        rng=np.random.default_rng(314)
        cases=[(rng.integers(-32768,32768,(31,29),np.int16),1),
               (rng.integers(0,2**32,(31,29),np.uint32),1),
               (rng.integers(0,65536,(31,29),np.uint16),0),
               (rng.normal(120,90,(31,29)).astype(np.float32),0)]
        for pixels,photo in cases:
            for big in [False,True]:
                if big and pixels.dtype.kind=='u':continue # Old Pillow has no MM rawmode for these domains.
                for deflate in [False,True]:
                    for orientation in [1,6]:
                        with self.subTest(dtype=str(pixels.dtype),photo=photo,big=big,deflate=deflate,orientation=orientation):
                            self.assert_image(self.raw_scalar_tiff(pixels,'extra-scalar.tif',photo=photo,
                                big_endian=big,deflate=deflate,orientation=orientation,rows=7))

    def test_tga_original_palette_thumbnail_and_bytes_format_detection(self):
        rgb=np.random.default_rng(735).integers(0,256,(1199,1057,3),np.uint8)
        for mode in ['P','RGB','RGBA','L','LA']:
            for compression in [None,'tga_rle']:
                with self.subTest(mode=mode,compression=compression):
                    self.assert_image(self.save(Image.fromarray(rgb).convert(mode),'mode.tga',compression=compression))

    def test_tga_handwritten_rgb16_la_descriptor_and_bilevel_original_modes(self):
        for kind,depth,data in ((2,16,b'\x00\x7c\xe0\x03'),(10,16,b'\x01\x00\x7c\xe0\x03'),
                                (3,16,b'\x80\xff\x40\x20'),(11,16,b'\x01\x80\xff\x40\x20'),
                                (3,1,b'\x80'),
                                # BGRA;15Z high bit set: Pillow makes those pixels transparent.
                                (2,16,b'\x04\xfc\xe4\x03'),(10,16,b'\x01\x04\xfc\xe4\x83')):
            for orientation in (0,16,32,48):
                header=bytearray(18);header[2]=kind;header[12:16]=struct.pack('<HH',2,1)
                header[16]=depth;header[17]=orientation
                path=self.root/f'hand-{kind}-{depth}-{orientation}.tga';path.write_bytes(header+data+bytes(8)+b'TRUEVISION-XFILE.\0')
                self.assert_image(str(path))

    def test_tga_handwritten_16bit_palette_original_raw_rle_and_orientation(self):
        for kind,data in ((1,b'\x00\x01'),(9,b'\x01\x00\x01')):
          for palette in (b'\x00\x7c\xe0\x03',b'\x04\xfc\xe4\x83'):
            for orientation in (0,1,8,16,17,24,32,33,40,48,49,56):
                header=bytearray(18);header[1]=1;header[2]=kind
                header[5:7]=struct.pack('<H',2);header[7]=16
                header[12:16]=struct.pack('<HH',2,1);header[16]=8;header[17]=orientation
                path=self.root/f'palette16-{kind}-{palette.hex()}-{orientation}.tga'
                path.write_bytes(header+palette+data+bytes(8)+b'TRUEVISION-XFILE.\0')
                self.assert_image(str(path))

    def test_ico_original_png_modes_frame_ranking_and_bmp32_alpha(self):
        def icon(entries):
            raw=b'\0\0\1\0'+struct.pack('<H',len(entries));directory=b'';payload=b''
            offset=6+16*len(entries)
            for image,bpp in entries:
                buf=io.BytesIO();image.save(buf,format='PNG');data=buf.getvalue()
                w,h=image.size
                directory+=struct.pack('<BBBBHHII',w%256,h%256,0,0,1,bpp,len(data),offset)
                payload+=data;offset+=len(data)
            path=self.root/'frames.ico';path.write_bytes(raw+directory+payload);return str(path)
        for mode in ['RGB','RGBA','L','LA','P','1']:
            with self.subTest(mode=mode):
                self.assert_image(icon([(picture(31,(128,128)).convert(mode),24)]))
        # Area before depth, ascending depth within equal area, first entry for exact ties.
        self.assert_image(icon([(Image.new('RGBA',(32,32),'blue'),32),(picture(8,(64,64)),24)]))
        self.assert_image(icon([(Image.new('RGBA',(64,64),'blue'),32),(picture(8,(64,64)),24)]))
        self.assert_image(icon([(picture(8,(64,64)),24),(Image.new('RGB',(64,64),'red'),24)]))
        rgba=picture(35,(64,64)).convert('RGBA')
        rgba.putalpha(Image.fromarray(np.random.default_rng(93).integers(0,256,(64,64),np.uint8)))
        path=self.save(rgba,'dib.ico',bitmap_format='bmp',sizes=[(64,64)])
        self.assert_image(path)
        raw=bytearray(Path(path).read_bytes());raw+=b'\xff'*(64*8)
        struct.pack_into('<I',raw,14,len(raw)-22)
        Path(path).write_bytes(raw);self.assert_image(path)

    def test_tiff_planar_and_associated16_keep_original_high_byte_unpack(self):
        pixels=np.random.default_rng(814).integers(0,65536,(19,17,4),np.uint16)
        pixels[::3,::2]=[600,400,1200,0]
        for big in [False,True]:
            for bands in [3,4]:
                for deflate in [False,True]:
                    with self.subTest(big=big,bands=bands,deflate=deflate):
                        self.assert_image(self.raw_byte_tiff(pixels[:,:,:bands],'planar16.tif',planar=True,
                            big_endian=big,deflate=deflate,tiled=True))
            for deflate in [False,True]:
                with self.subTest(big=big,associated=True,deflate=deflate):
                    self.assert_image(self.raw_byte_tiff(pixels,'associated16.tif',associated=True,
                        big_endian=big,deflate=deflate))

    def test_tiff_packed12_original_unsigned_samples_and_thumbnail(self):
        rng=np.random.default_rng(816)
        for shape in [(19,17),(1101,1297)]:
            pixels=rng.integers(0,4096,shape,np.uint16)
            for deflate in [False,True]:
                for orientation in [1,6]:
                    with self.subTest(shape=shape,deflate=deflate,orientation=orientation):
                        self.assert_image(self.raw_scalar_tiff(pixels,'packed12.tif',bits=12,
                            deflate=deflate,orientation=orientation,rows=7))

    def test_tiff_mm32_lzw_packbits_retain_original_native_word_reinterpretation(self):
        rng=np.random.default_rng(818)
        for shape in [(31,29),(1101,1297)]:
            arrays=[rng.integers(-1000,1000,shape,np.int32),rng.normal(120,90,shape).astype(np.float32)]
            for values in arrays:
                for codec in ['lzw','packbits']:
                    for orientation in [1,6]:
                        with self.subTest(shape=shape,dtype=str(values.dtype),codec=codec,orientation=orientation):
                            self.assert_image(self.raw_scalar_tiff(values,'mm32-other-codecs.tif',big_endian=True,
                                codec=codec,orientation=orientation,rows=7))

    def test_tiff_i32_cubic_overshoot_keeps_original_platform_storage(self):
        x=np.arange(1025)
        pixels=np.where((x//20)%4==1,np.iinfo(np.int32).max,
                        np.where((x//20)%4==3,np.iinfo(np.int32).min,0)).astype(np.int32)[None,:]
        # The old scalar thumbnail retains I32 between axes. macOS saturates;
        # Windows MSVC stores INT_MIN for positive overflow, before RGB clamp.
        self.assert_image(self.raw_scalar_tiff(pixels,'i32-overflow.tiff'))

    def test_tiff_subbyte_gray_palette_and_fill_order_original_modes(self):
        rng=np.random.default_rng(819)
        for bits in [1,2,4,8]:
            pixels=rng.integers(0,1<<bits,(31,29),np.uint8)
            palette=[((i*193)%256)*257 for c in range(3) for i in range(1<<bits)]
            for photo in [0,1,3]:
                for fill in [1,2]:
                    for codec in [None,'lzw']:
                        with self.subTest(bits=bits,photo=photo,fill=fill,codec=codec):
                            path=self.raw_scalar_tiff(pixels,'subbyte.tif',bits=bits,
                                photo=photo,fill_order=fill,codec=codec,rows=7,
                                palette=palette if photo==3 else None)
                            if fill==2 and codec is None and (photo==3 and bits<8 or photo==0 and bits==8):
                                with self.assertRaisesRegex(ValueError,'unknown raw mode'):
                                    relate.load(path)
                                request,response=self.root/'invalid.json',self.root/'invalid-out.json'
                                request.write_text(json.dumps(dict(op='load',path=path)))
                                run=subprocess.run([str(self.binary),str(request),str(response)],env=os.environ|{'PATH':''},capture_output=True,timeout=30)
                                self.assertNotEqual(run.returncode,0);self.assertIn('unknown raw mode',run.stderr.decode());self.assertFalse(response.exists())
                            else:
                                self.assert_image(path)

    def test_tiff_group4_original_bilevel_and_compressed_fill_order(self):
        image=picture(81,(59,87)).convert('1')
        for fill in [1,2]:
            with self.subTest(fill=fill):
                self.assert_image(self.save(image,'fax.tif',compression='group4',tiffinfo={266:fill}))

    def test_tiff_compressed_u16_fill_order_original_samples(self):
        y,x=np.mgrid[:17,:19]
        im=Image.fromarray(((x*317+y*701)%65536).astype(np.uint16))
        for codec in ('tiff_adobe_deflate','tiff_lzw','packbits'):
            self.assert_image(self.save(im, 'fill-u16-'+codec+'.tiff',compression=codec,tiffinfo={266:2}))

    def test_tiff_fill_order_shared_encoded_strip_is_reversed_once(self):
        pixels=np.array([[7,120,240],[7,120,240]],np.uint8)
        path=self.raw_scalar_tiff(pixels,'shared-fill.tiff',bits=8,fill_order=2,deflate=True,rows=1)
        data=bytearray(Path(path).read_bytes())
        count=struct.unpack_from('<H',data,8)[0]
        for i in range(count):
            at=10+12*i
            if struct.unpack_from('<H',data,at)[0]==273:
                offsets=struct.unpack_from('<I',data,at+8)[0]
                first=struct.unpack_from('<I',data,offsets)[0]
                struct.pack_into('<I',data,offsets+4,first)
        Path(path).write_bytes(data)
        self.assert_image(path)

    def test_tiff_jpeg_uses_original_idct_and_raw_cmyk_before_thumbnail(self):
        y,x=np.mgrid[:17,:19]
        im=Image.fromarray(np.stack(((x*17+y*31)%256,(x*31+y*7)%256,(x*11+y*19)%256),axis=2).astype(np.uint8))
        for mode in ('RGB','L','CMYK'):
            for orientation in (1,6):
                self.assert_image(self.save(im.convert(mode),f'jpeg-{mode}-{orientation}.tiff',compression='jpeg',tiffinfo={274:orientation}))

    def test_tiff_packed12_odd_tile_local_bits_and_original_edge_stride(self):
        for cw in (3,16):
            w,h,ch=cw+2,2,2
            values=[[10*(1+x+y*w) for x in range(w)] for y in range(h)]
            blocks=[]
            for left in (0,cw):
                block=bytearray()
                for row in values:
                    packed=bytearray((cw*12+7)//8)
                    for x in range(cw):
                        v=row[left+x] if left+x<w else 0
                        n=v<<(4-x*12%8);at=x*12//8
                        packed[at]|=n>>8;packed[at+1]|=n&255
                    block+=packed
                blocks.append(bytes(block))
            fields=[(256,4,1,w),(257,4,1,h),(258,3,1,12),(259,3,1,1),(262,3,1,1),
                    (277,3,1,1),(322,4,1,cw),(323,4,1,ch),(324,4,2,0),(325,4,2,0)]
            base=8+2+12*len(fields)+4;payload=base+16
            raw=b'II'+struct.pack('<HIH',42,8,len(fields))
            for tag,typ,count,val in fields:
                if tag==324:val=base
                if tag==325:val=base+8
                raw+=struct.pack('<HHI',tag,typ,count)+(struct.pack('<H',val)+b'\0\0' if typ==3 else struct.pack('<I',val))
            raw+=struct.pack('<I',0)+struct.pack('<II',payload,payload+len(blocks[0]))+struct.pack('<II',*[len(b) for b in blocks])+b''.join(blocks)
            path=self.root/f'packed-tile-{cw}.tiff';path.write_bytes(raw)
            self.assert_image(str(path))

    def test_mode_f_resize_exact_float32_stages(self):
        rng = np.random.default_rng(12)
        for shape, size in [((31, 29, 3), (13, 17)), ((29, 31, 3), (64, 65)),
                            ((1, 37, 3), (20, 1)), ((33, 19), (7, 81)),
                            ((2048, 6, 3), (3, 1024)), ((2048, 6), (3, 1024)),
                            ((1200, 12, 3), (6, 600)), ((1201, 12, 3), (6, 600))]:
            with self.subTest(shape=shape, size=size):
                x = rng.uniform(-30, 290, shape).astype(np.float32)
                image = dict(width=shape[1], height=shape[0], channels=shape[2] if len(shape)==3 else 1,
                             pixels=x.ravel().tolist())
                got = self.probe('resize', image=image, width=size[0], height=size[1])
                expected = relate._resize(x, size)
                np.testing.assert_array_equal(np.asarray(got['bits'], np.uint32), expected.view(np.uint32).ravel())

    def test_thin_portrait_original_axis_order_byte_alpha_and_integer_modes(self):
        rng=np.random.default_rng(15219)
        # The old Image.resize branch is strict height > width*100 and applies
        # after reducing_gap. Cover equality, shrinking both axes and reduction.
        for shape in [(1200,12),(1201,12),(2048,6),(7001,25)]:
            for mode,channels in [('L',1),('RGB',3),('RGBA',4),('LA',2)]:
                with self.subTest(shape=shape,mode=mode):
                    pixels=rng.integers(0,256,(*shape,channels),np.uint8)
                    im=Image.fromarray(pixels[...,0] if channels==1 else pixels,mode)
                    self.assert_image(self.save(im,f'thin-{shape}-{mode}.png'))
        # Handwritten counterexample: each 3x3 block sums to 5. The old
        # UINT32 reciprocal yields 0; exact (sum+4)//9 would incorrectly yield 1.
        tile=np.array([[5,0,0],[0,0,0],[0,0,0]],np.uint8)
        for mode in ['L','RGB']:
            path=self.save(Image.fromarray(np.tile(tile,(2048,8))).convert(mode),f'reduce-nine-{mode}.png')
            expected,_=relate.load(path)
            self.assertEqual(set(expected.ravel()),{0})
            self.assert_image(path)
        pixels=rng.integers(0,65536,(2048,6),np.uint16)
        self.assert_image(self.save(Image.fromarray(pixels),'thin-gray16.png'))

    def test_review_original_png_bands_gray16_resize_and_bmp_palette_modes(self):
        # Independent review counterexamples. Old Pillow outputs remain authoritative.
        for mode, colour, key, grey in [('RGB','red',(255,0,0),76),('L',128,128,128)]:
            path=self.save(Image.new(mode,(32,32),colour),mode+'-trns.png',transparency=key)
            self.assertEqual(set(similar.signature(path)[1]),{grey})
            self.assert_image(path)
        for peak in [510,65535]:
            raw=np.tile(np.array([0,peak],np.uint16),2048).reshape(2,2048)
            for fmt in ['PNG','TIFF']:
                self.assert_image(self.save(Image.fromarray(raw),f'gray16-{peak}.{fmt.lower()}',format=fmt))
        raw=np.tile(np.array([0,255],np.uint8),2048).reshape(2,2048)
        self.assert_image(self.save(Image.fromarray(raw),'gray.bmp'))
        palette=Image.fromarray(raw).convert('P')
        palette.putpalette([v for i in range(256) for v in (255-i,255-i,255-i)])
        self.assert_image(self.save(palette,'palette.bmp'))
        # Old I;16 thumbnail's reducing_gap rejects this mode via Image.reduce.
        raw=np.tile(np.array([0,65535],np.uint16),8192).reshape(2,8192)
        path=self.save(Image.fromarray(raw),'gray16-reduce.png')
        with self.assertRaisesRegex(ValueError,'image has wrong mode'):
            relate.load(path)
        request,response=self.root/'probe.json',self.root/'out.json'
        request.write_text(json.dumps(dict(op='load',path=path)),encoding='utf-8')
        run=subprocess.run([str(self.binary),str(request),str(response)],env=os.environ|{'PATH':''},capture_output=True,timeout=10)
        self.assertNotEqual(run.returncode,0)
        self.assertIn('image has wrong mode',run.stderr.decode())

    def test_tiff_filename_mmap_and_bytes_have_their_own_original_orientation(self):
        values=np.arange(6,dtype=np.uint16).reshape(2,3)*32
        for mode in ['L','I;16','RGB','RGBA','RGBX','CMYK']:
            im=Image.fromarray(values if mode=='I;16' else values.astype(np.uint8))
            im=im if mode=='I;16' else im.convert(mode)
            for compression in ['raw','tiff_lzw','tiff_adobe_deflate']:
                for orientation in [1,3,5,6,7,8]:
                    with self.subTest(mode=mode,compression=compression,orientation=orientation):
                        exif=Image.Exif();exif[274]=orientation
                        path=self.save(im,f'{mode}-{compression}-{orientation}.tiff',compression=compression,exif=exif)
                        self.assert_image(path)
        # Handwritten invariant: this raw filename mmap is observably different
        # from BytesIO; do not invent equality or unify the old outputs.
        exif=Image.Exif();exif[274]=5
        path=self.save(Image.fromarray(values),'mmap.tiff',exif=exif)
        self.assertEqual(similar.signature(path)[0],1.5)
        self.assertEqual(similar.signature(io.BytesIO(Path(path).read_bytes()))[0],2/3)

    def test_gif_partial_first_frame_original_palette_zero_and_transparency(self):
        for transparent in [None,0,1]:
            for local in [False,True]:
                with self.subTest(transparent=transparent,local=local):
                    im=Image.new('P',(2,2),1)
                    palette=[255,0,0,0,255,0]+[0]*762
                    im.putpalette(palette)
                    stream=io.BytesIO()
                    options={} if transparent is None else dict(transparency=transparent)
                    im.save(stream,format='GIF',optimize=False,**options)
                    raw=bytearray(stream.getvalue());raw[6:10]=bytes([4,0,4,0])
                    table=3*(2**((raw[10]&7)+1)) if raw[10]&128 else 0
                    pos=raw.index(0x2c,13+table);raw[pos+1:pos+5]=bytes([1,0,1,0])
                    if local:
                        raw[pos+9]=0x87
                        raw[pos+10:pos+10]=bytes([0,0,255,0,255,0]+[0]*762)
                    path=self.root/f'partial-{transparent}-{local}.gif';path.write_bytes(raw)
                    self.assert_image(str(path))
                    from provenance.verify import _colour
                    corner = [0,255,0] if transparent == 1 else ([0,0,255] if local else [255,0,0])
                    self.assertEqual(_colour(path)[0,0].tolist(),corner)

    def test_cmyk_tiff_raw_conversion_resize_reduce_and_both_original_entrances(self):
        rng=np.random.default_rng(152)
        for shape in [(2,3),(6,2048),(24,8192),(7001,25)]:
            raw=rng.integers(0,256,(*shape,4),np.uint8)
            raw[0,:3]=[[31,127,210,90],[255,0,20,129],[4,66,128,17]]
            im=Image.fromarray(raw,'CMYK')
            for compression in ['raw','tiff_lzw','tiff_adobe_deflate']:
                with self.subTest(shape=shape,compression=compression):
                    path=self.save(im,f'cmyk-{shape}-{compression}.tiff',compression=compression)
                    self.assert_image(path)
        # Integer Pillow conversion, independently handwritten counterexample
        # to image's float truncation. It must survive the TIFF decoding path.
        from provenance.verify import _colour
        im=Image.new('CMYK',(24,24),(31,127,210,90))
        path=self.save(im,'cmyk-rounding.tiff')
        self.assertEqual(_colour(path)[0,0].tolist(),[145,83,29])
        self.assert_image(path)

    def test_handwritten_png_original_mode_depth_and_transparency_matrix(self):
        rng=np.random.default_rng(15216)
        for colour,channels,depths in [(0,1,[1,2,4,8,16]),(2,3,[8,16]),
                                       (3,1,[1,2,4,8]),(4,2,[8,16]),(6,4,[8,16])]:
            for depth in depths:
                for transparent in ([False,True] if colour in [0,2,3] else [False]):
                    with self.subTest(colour=colour,depth=depth,transparent=transparent):
                        pixels=rng.integers(0,1<<depth,(7,13,channels),np.uint16)
                        pixels[0,0]=0
                        tr=(bytes([0,128]) if colour==3 else struct.pack('>'+'H'*channels,*([0]*channels))) if transparent else None
                        self.assert_image(self.raw_png(pixels,colour,depth,f'raw-{colour}-{depth}-{transparent}.png',transparency=tr))
        for colour,channels in [(2,3),(4,2),(6,4)]:
            pixels=rng.integers(0,65536,(6,2048,channels),np.uint16)
            self.assert_image(self.raw_png(pixels,colour,16,f'wide16-{colour}.png'))
        pixels=np.array([[[510,128,65279]]],np.uint16)
        path=self.raw_png(pixels,2,16,'high-byte.png')
        from provenance.verify import _colour
        self.assertEqual(_colour(path)[0,0].tolist(),[1,0,254])
        self.assert_image(path)

    def test_handwritten_rgb16_tiff_and_big_endian_gray16_original_resampler(self):
        rng=np.random.default_rng(15217)
        for channels in [3,4]:
            for endian in [False,True]:
                for shape in [(2,3),(6,2048)]:
                    with self.subTest(channels=channels,endian=endian,shape=shape):
                        pixels=rng.integers(0,65536,(*shape,channels),np.uint16)
                        self.assert_image(self.raw_rgb16_tiff(pixels,f'rgb16-{channels}-{endian}-{shape}.tiff',big_endian=endian))
        for compression in ['raw','tiff_lzw','tiff_adobe_deflate']:
            for shape in [(2,3),(6,2048),(2048,6)]:
                pixels=rng.integers(0,65536,shape,np.uint16)
                im=Image.frombytes('I;16B',(shape[1],shape[0]),pixels.astype('>u2').tobytes())
                with self.subTest(mode='I;16B',compression=compression,shape=shape):
                    self.assert_image(self.save(im,f'gray16b-{compression}-{shape}.tiff',compression=compression))
        # Pillow's existing native-storage resampler in I;16B makes interior
        # 0/65535 alternating pixels 128, not 32768, after the retained mode read.
        pixels=np.tile(np.array([0,65535],np.uint16),2048).reshape(2,2048)
        im=Image.frombytes('I;16B',(2048,2),pixels.astype('>u2').tobytes())
        path=self.save(im,'gray16b-storage.tiff')
        expected,_=relate.load(path)
        self.assertEqual(expected.min(),128)
        self.assert_image(path)

    def test_tiff_scalar_signed_float_original_resize_reduce_and_source_type_rounds(self):
        rng=np.random.default_rng(15232)
        for mode in ['I','F']:
            for shape in [(2,3),(6,2048),(2048,6),(6,8192),(7001,25)]:
                values=(rng.integers(-65536,65537,shape,np.int32) if mode=='I'
                        else rng.uniform(-1024,1024,shape).astype(np.float32))
                im=Image.fromarray(values)
                for compression in ['raw','tiff_lzw','tiff_adobe_deflate']:
                    with self.subTest(mode=mode,shape=shape,compression=compression):
                        self.assert_image(self.save(im,f'scalar-{mode}-{shape}-{compression}.tiff',compression=compression))
        # Handwritten source-type counterexamples: I32 full groups wrap before
        # double accumulation; F32 full groups round before double accumulation.
        signed=np.full((6,8192),np.iinfo(np.int32).max,np.int32)
        floats=np.tile(np.array([[16777216.,1.],[-16777216.,3.]],np.float32),(2,2048))
        for mode,values in [('I',signed),('F',floats)]:
            path=self.save(Image.fromarray(values),f'scalar-group-{mode}.tiff')
            expected,_=relate.load(path)
            self.assertEqual(set(expected.ravel()),{0})
            self.assert_image(path)
        # The MM compressed 32-bit rawmode conversion precedes thumbnail/reduce,
        # not only the final clamp. Encode swapped words of finite target values
        # so the independent old decoder restores those values before resampling.
        for shape in [(6,2048),(6,8192)]:
            for mode in ['I','F']:
                target=(rng.integers(-1024,1025,shape,np.int32) if mode=='I'
                        else rng.uniform(-1024,1024,shape).astype(np.float32))
                values=target.view(np.uint32).byteswap().view(target.dtype)
                path=self.raw_scalar_tiff(values,f'mm-compressed-before-resize-{mode}-{shape}.tiff',big_endian=True,deflate=True)
                with Image.open(path) as im:
                    np.testing.assert_array_equal(np.asarray(im).view(np.uint32),target.view(np.uint32))
                self.assert_image(path)

    def test_handwritten_tiff_scalar_endian_strips_orientation_fill_and_nonfinite(self):
        # Fixed old-decoder words: raw MM reads its original values, whereas
        # compressed MM libtiff retains the big-endian 32-bit rawmode after its
        # native-endian decode. Expectations are handwritten, never candidate output.
        signed=np.array([[np.iinfo(np.int32).min,-1,0],[127,256,np.iinfo(np.int32).max]],np.int32)
        floats=np.array([[1,2,3],[4,100,255]],np.float32)
        signed_compressed=[128,-1,0,2130706432,65536,-129]
        float_compressed_bits=[32831,64,16448,32832,51266,32579]
        for values in [signed,floats]:
            for deflate in [False,True]:
                for rows in [None,1]:
                    for icc in [None,b'synthetic ICC']:
                        name=f'mm-fixed-{values.dtype}-{deflate}-{rows}-{icc is not None}.tiff'
                        path=self.raw_scalar_tiff(values,name,big_endian=True,deflate=deflate,rows=rows,icc=icc)
                        with Image.open(path) as im:
                            decoded=np.asarray(im)
                            if not deflate:
                                np.testing.assert_array_equal(decoded.view(np.uint32),values.view(np.uint32))
                            elif values.dtype.kind=='i':
                                self.assertEqual(decoded.ravel().tolist(),signed_compressed)
                            else:
                                self.assertEqual(decoded.view(np.uint32).ravel().tolist(),float_compressed_bits)
                        self.assert_image(path)
        arrays=[np.array([[0,127,256],[510,32768,65535]],np.uint16),
                np.array([[np.iinfo(np.int32).min,-1,0],[127,256,np.iinfo(np.int32).max]],np.int32),
                np.array([[np.nan,-np.inf,np.inf],[-0.25,0.75,127.75]],np.float32)]
        for values in arrays:
            for big in [False,True]:
                for deflate in [False,True]:
                    for rows in [None,1]:
                        for orientation in [1,2,3,4,5,6,7,8,9]:
                            with self.subTest(dtype=str(values.dtype),big=big,deflate=deflate,rows=rows,orientation=orientation):
                                name=f'raw-scalar-{values.dtype}-{big}-{deflate}-{rows}-{orientation}.tiff'
                                self.assert_image(self.raw_scalar_tiff(values,name,big_endian=big,orientation=orientation,rows=rows,deflate=deflate))
        values=np.array([[0,1,127],[255,256,65535]],np.uint16)
        for rows in [None,1]:
            for orientation in [1,5,6,8]:
                path=self.raw_scalar_tiff(values,f'fill2-{rows}-{orientation}.tiff',fill_order=2,rows=rows,orientation=orientation)
                self.assert_image(path)
        # Unknown orientations stay unchanged in the old implementation.
        self.assert_image(self.raw_scalar_tiff(values,'unknown-orientation.tiff',orientation=65535))
        # Former candidate-only rejection domains now require full original
        # pixels/signature/colour parity through all five consumer entrances.
        for values in [signed,floats]:
            for unsupported in ['codec32946','planar2']:
                path=self.raw_scalar_tiff(values,f'mm-unsupported-{values.dtype}-{unsupported}.tiff',big_endian=True,
                                          deflate=True,icc=None,planar=2 if unsupported=='planar2' else 1)
                if unsupported=='codec32946':
                    raw=bytearray(Path(path).read_bytes())
                    count=struct.unpack_from('>H',raw,8)[0]
                    for offset in range(10,10+count*12,12):
                        if struct.unpack_from('>H',raw,offset)[0]==259:
                            struct.pack_into('>H',raw,offset+8,32946)
                            break
                    else:self.fail('handwritten Compression tag missing')
                    Path(path).write_bytes(raw)
                self.assert_image(path)

    def test_pnm_original_maxval_rounding_i_mode_thumbnail_and_bitmap_nearest(self):
        rng=np.random.default_rng(15218)
        def write(magic,maximum,pixels,name):
            height,width,bands=pixels.shape
            values=pixels.ravel().tolist()
            header=magic+f'\n# independent synthetic sample\n{width} {height}\n{maximum}\n'.encode()
            plain=magic in [b'P2',b'P3']
            data=(' '.join(map(str,values))+'\n').encode() if plain else (
                bytes(values) if maximum<=255 else struct.pack('>'+'H'*len(values),*values))
            path=self.root/name;path.write_bytes(header+data);return str(path)
        for magic,bands in [(b'P2',1),(b'P3',3),(b'P5',1),(b'P6',3)]:
            for maximum in [6,10,17,255,256,1000,65535]:
                with self.subTest(magic=magic,maximum=maximum):
                    pixels=rng.integers(0,maximum+1,(5,7,bands),np.uint16)
                    pixels[0,:3]=np.array([1,maximum//2,maximum])[:,None]
                    self.assert_image(write(magic,maximum,pixels,f'{magic.decode()}-{maximum}.pnm'))
        for magic in [b'P2',b'P5']:
            for shape in [(6,2048),(25,7001),(2048,6),(7001,25)]:
                values=np.resize(np.array([0,1,2,10,1000],np.uint16),(*shape,1))
                self.assert_image(write(magic,1000,values,f'i-mode-{magic.decode()}-{shape}.pnm'))
        for magic in [b'P3',b'P6']:
            values=np.ones((1,1,3),np.uint16)
            path=write(magic,6,values,f'ties-{magic.decode()}.pnm')
            from provenance.verify import _colour
            self.assertEqual(_colour(path)[0,0].tolist(),[42,42,42])
            self.assert_image(path)
        values=rng.integers(0,2,(3,2048),np.uint8)
        for magic in [b'P1',b'P4']:
            header=magic+b'\n2048 3\n'
            data=(' '.join(map(str,values.ravel()))+'\n').encode() if magic==b'P1' else np.packbits(values,axis=1).tobytes()
            path=self.root/(magic.decode()+'.pbm');path.write_bytes(header+data)
            self.assert_image(str(path))

    def test_legacy_classification_crop_boxes_notes_and_changed_share(self):
        base = picture(1)
        a = self.save(base, 'a.png')
        cases = [('identical', base.copy()),
                 ('duplicate', jpeg(base.resize((600, 800), Image.Resampling.LANCZOS), 70))]
        variant = base.copy()
        variant.paste(picture(7, (260, 220)), (330, 420))
        cases.append(('variant', jpeg(variant, 90)))
        tiny = base.copy()
        tiny.paste(picture(8, (90, 90)), (400, 500))
        cases.append(('subtle', jpeg(tiny, 90)))
        mosaic = base.copy()
        mosaic.paste(base.crop((300, 400, 600, 700)).resize((10, 10), Image.Resampling.NEAREST)
                     .resize((300, 300), Image.Resampling.NEAREST), (300, 400))
        cases.append(('edit', mosaic))
        fill = base.copy()
        ImageDraw.Draw(fill).rectangle((300, 400, 600, 700), fill=(90, 80, 70))
        cases.append(('edit', fill))
        border = base.copy()
        ImageDraw.Draw(border).rectangle((0, 1140, 900, 1200), fill='white')
        cases.append(('edit', border))
        tone = Image.fromarray(np.clip(np.asarray(base, np.float32)*.8+30, 0, 255).astype(np.uint8))
        cases.append(('edit', tone))
        cases.append(('crop', jpeg(base.crop((180, 300, 720, 900)).resize((360, 400), Image.Resampling.LANCZOS), 85)))
        cases.append(('unrelated', picture(2)))
        for i, (kind, image) in enumerate(cases):
            with self.subTest(kind=kind, sample=i):
                b = self.save(image, f'b{i}.png')
                expected = relate.relate(relate.load(a), relate.load(b))
                self.assertEqual(expected['kind'], kind, expected)
                actual = self.probe('relate', a=a, b=b)
                self.assertEqual(actual, expected)

    def test_locate_flat_is_rejected_and_synthetic_crop_reference_is_exact(self):
        base = picture(9, (600, 800))
        crop = base.crop((120, 200, 480, 600))
        a, b = self.save(base, 'base.png'), self.save(crop, 'crop.png')
        expected = relate.locate(relate.load(a)[0], relate.load(b)[0])
        actual = self.probe('locate', a=a, b=b)
        self.assertEqual(actual, list(expected))
        flat = self.save(Image.new('RGB', (120, 160), '#888888'), 'flat.png')
        self.assertIsNone(self.probe('locate', a=a, b=flat))


class NativeSimilarSnapshotTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_RESEARCH_COMPUTE_BIN')
        if not configured:
            raise unittest.SkipTest('explicit candidate research calculator required')
        cls.binary = Path(configured).resolve()
        if not cls.binary.is_file():
            raise AssertionError('configured candidate research calculator missing')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        with sqlite3.connect(self.root/'source.sqlite') as c:
            c.execute('CREATE TABLE embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))')
        with sqlite3.connect(self.root/'library.sqlite') as c:
            c.executescript('''CREATE TABLE assets(uuid TEXT PRIMARY KEY,src_path TEXT);
                CREATE TABLE excluded(uuid TEXT PRIMARY KEY);
                CREATE TABLE image_sigs(uuid TEXT PRIMARY KEY,ratio REAL,grid BLOB,ok INTEGER);
                CREATE TABLE relations(a TEXT,b TEXT,kind TEXT,changed REAL,note TEXT,
                    checked_at TEXT DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(a,b));
                CREATE TABLE duplicates(uuid TEXT PRIMARY KEY,grp TEXT);''')
        self.base = picture(1, (600, 800))
        for ident, image in [('a', self.base), ('b', jpeg(self.base, 80)),
                              ('c', picture(2, (600, 800)))]:
            path = self.root/(ident+'.png')
            image.save(path)
            with sqlite3.connect(self.root/'library.sqlite') as c:
                c.execute('INSERT INTO assets VALUES (?,?)', (ident, str(path)))

    def request(self, kind='similar', ids=None):
        request = dict(format='illustash-research/1', identity=dict(synthetic=True), version='fixed-v1',
                       kind=kind, args=dict(ids=ids or []), regroup=False, snapshot=str(self.root),
                       features=str(self.root/'features'), anchors=str(self.root/'anchors'))
        input_path, output = self.root/'request.json', self.root/'response.json'
        input_path.write_text(json.dumps(request), encoding='utf-8')
        run = subprocess.run([str(self.binary), str(input_path), str(output)],
                             env=os.environ | dict(PATH='', ILLUSTASH_RESEARCH_PROGRESS=str(self.root/'progress.json')),
                             capture_output=True, timeout=180)
        return run, output

    def test_exact_cache_rows_incremental_skip_retry_and_unchanged_snapshot(self):
        from web.research_snapshot import Snapshot
        reference = self.root/'reference'
        reference.mkdir()
        for name in ['source.sqlite', 'library.sqlite']:
            with sqlite3.connect(self.root/name) as src, sqlite3.connect(reference/name) as dest:
                src.backup(dest)
        def compare():
            before = (self.root/'library.sqlite').read_bytes()
            run, output = self.request()
            self.assertEqual(run.returncode, 0, run.stderr.decode())
            self.assertEqual((self.root/'library.sqlite').read_bytes(), before)
            out = json.loads(output.read_text())
            scanner = similar.SimilarScanner(Snapshot(reference))
            scanner.scan()
            self.assertEqual({k:out['result'][k] for k in ('status','compared','found')},
                             {k:scanner.state[k] for k in ('status','compared','found')})
            with sqlite3.connect(reference/'library.sqlite') as c:
                for table, rows in out['tables'].items():
                    expected = [list(r) for r in c.execute('SELECT * FROM '+table)]
                    if table=='image_sigs':
                        expected = [[r[0],r[1],r[2].hex() if r[2] is not None else None,r[3]] for r in expected]
                    if table=='relations':
                        rows = [r[:-1] for r in rows]
                        expected = [r[:-1] for r in expected]
                    self.assertEqual(rows, expected, table)
            # Simulate only App's accepted cache publication; no native candidate generates expectations.
            with sqlite3.connect(self.root/'library.sqlite') as dest, sqlite3.connect(reference/'library.sqlite') as src:
                src.backup(dest)
            return out
        first = compare()
        self.assertEqual([(r[0],r[1],r[2]) for r in first['tables']['relations']], [('a','b','duplicate')])
        self.assertEqual(compare()['result']['compared'], 0)
        for root in [self.root, reference]:
            with sqlite3.connect(root/'library.sqlite') as c:
                c.execute('DELETE FROM relations')
                c.execute("INSERT INTO similar_retry VALUES ('a','b')")
                c.execute("UPDATE assets SET src_path=src_path||'.missing' WHERE uuid='b'")
        failed = compare()
        self.assertEqual(failed['tables']['similar_retry'], [['a','b']])
        for root in [self.root, reference]:
            with sqlite3.connect(root/'library.sqlite') as c:
                c.execute("UPDATE assets SET src_path=replace(src_path,'.missing','') WHERE uuid='b'")
        self.assertEqual(compare()['tables']['similar_retry'], [])

    def test_relate_deduplicates_requested_ids_missing_errors_and_read_only_snapshot(self):
        before = (self.root/'library.sqlite').read_bytes()
        run, output = self.request('relate', ['b','a','b','unknown'])
        self.assertEqual(run.returncode, 0, run.stderr.decode())
        result = json.loads(output.read_text())['result']
        expected = relate.relate_all({u:relate.load(self.root/(u+'.png')) for u in ['b','a']}) | dict(missing=['unknown'])
        self.assertEqual(result, expected)
        self.assertEqual(json.dumps(result, ensure_ascii=False), json.dumps(expected, ensure_ascii=False))
        self.assertEqual((self.root/'library.sqlite').read_bytes(), before)
        run, _ = self.request('relate', ['a','unknown'])
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('能读到的图不足两张', run.stderr.decode())

    def test_relate_mode_failure_is_not_demoted_to_missing(self):
        path = self.root/'large-uint16.png'
        Image.new('I;16', (4096,4096), 600).save(path)
        with self.assertRaisesRegex(ValueError, 'image has wrong mode'):
            relate.load(path)
        with sqlite3.connect(self.root/'library.sqlite') as c:
            c.execute('INSERT INTO assets VALUES (?,?)', ('mode-error',str(path)))
        before = (self.root/'library.sqlite').read_bytes()
        run, output = self.request('relate', ['a','b','mode-error'])
        self.assertNotEqual(run.returncode, 0)
        self.assertIn('image has wrong mode', run.stderr.decode())
        self.assertFalse(output.exists())
        self.assertEqual((self.root/'library.sqlite').read_bytes(), before)
        path.write_bytes(b'not an image')
        run, output = self.request('relate', ['a','b','mode-error'])
        self.assertEqual(run.returncode, 0, run.stderr.decode())
        self.assertEqual(json.loads(output.read_text())['result']['missing'], ['mode-error'])

    def test_duplicate_excluded_and_historical_cache_rows_are_kept(self):
        with sqlite3.connect(self.root/'library.sqlite') as c:
            c.execute("INSERT INTO duplicates VALUES ('a','copies'),('b','copies')")
            c.execute("INSERT INTO excluded VALUES ('c')")
            c.execute("INSERT INTO image_sigs VALUES ('removed',1,?,1)", (bytes(range(256)),))
            c.execute("INSERT INTO relations VALUES ('old-a','old-b','duplicate',0,'historic','2000-01-01')")
        run, output = self.request()
        self.assertEqual(run.returncode, 0, run.stderr.decode())
        out = json.loads(output.read_text())
        self.assertEqual(out['result']['compared'], 0)
        self.assertEqual(out['tables']['relations'], [['old-a','old-b','duplicate',0,'historic','2000-01-01']])
        self.assertEqual([r[0] for r in out['tables']['image_sigs']], ['removed','a','b'])
        self.assertEqual(out['tables']['similar_paired'], [['a'],['b']])

    def test_candidate_blocks_precise_ccip_threshold_and_uuid_ranking_reference(self):
        from web.features import Features
        rng = np.random.default_rng(51)
        ids = [f'{i:04}' for i in range(520)][::-1]
        raw = np.zeros((520,768), np.float32)
        raw[np.arange(520), np.arange(520)+2] = 1
        raw[:15] = 0
        raw[:15,0] = 1
        for i, score in enumerate([np.nextafter(np.float32(.988),np.float32(0)),
                                  np.float32(.988), np.nextafter(np.float32(.988),np.float32(1))], 15):
            raw[i] = 0
            raw[i,0] = score
            raw[i,1] = np.sqrt(np.float32(1)-score*score)
        grids = [r.tobytes() for r in rng.integers(0,256,(520,256),np.uint8)]
        grids[:15] = [bytes(range(256))]*15
        ratios = [1.0]*520
        with sqlite3.connect(self.root/'library.sqlite') as c:
            c.execute('DELETE FROM assets')
            c.executemany('INSERT INTO assets VALUES (?,?)', [(u,str(self.root/'missing.png')) for u in ids])
            c.executemany('INSERT INTO image_sigs VALUES (?,?,?,1)', zip(ids,ratios,grids))
        with sqlite3.connect(self.root/'source.sqlite') as c:
            c.executemany("INSERT INTO embeddings VALUES (?,'ccip',?)", [(u,v.astype('<f4').tobytes()) for u,v in zip(ids,raw)])
        features = Features(self.root/'source.sqlite', base_dir=self.root/'features').load()
        for query in [None, {ids[0],ids[15],ids[16],ids[17]}]:
            if query is not None:
                with sqlite3.connect(self.root/'library.sqlite') as c:
                    c.executescript('CREATE TABLE IF NOT EXISTS similar_paired(uuid TEXT PRIMARY KEY);'
                                    'CREATE TABLE IF NOT EXISTS similar_retry(a TEXT,b TEXT,PRIMARY KEY(a,b));'
                                    'DELETE FROM similar_paired;DELETE FROM similar_retry;')
                    c.executemany('INSERT INTO similar_paired VALUES (?)', [(u,) for u in ids if u not in query])
            expected = similar.grid_pairs(ids,ratios,grids,query) | similar.ccip_pairs(features,ids,query)
            run, output = self.request()
            self.assertEqual(run.returncode, 0, run.stderr.decode())
            out = json.loads(output.read_text())
            self.assertEqual(out['tables']['similar_retry'], [list(p) for p in sorted(expected)])
            self.assertEqual(out['result']['compared'], len(expected))
            self.assertEqual(out['tables']['relations'], [])


if __name__=='__main__':
    unittest.main()
