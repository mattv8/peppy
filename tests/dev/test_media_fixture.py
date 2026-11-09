import binascii
import struct
import unittest
import zlib
from pathlib import Path


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
FIXTURE = Path(__file__).resolve().parents[1] / "fixtures/media/valid-1x1.png"


class MediaFixtureTests(unittest.TestCase):
    def test_valid_1x1_png_has_verified_chunks_and_decompressible_pixels(self) -> None:
        data = FIXTURE.read_bytes()
        self.assertTrue(data.startswith(PNG_SIGNATURE))

        offset = len(PNG_SIGNATURE)
        chunks: list[tuple[bytes, bytes]] = []
        while offset < len(data):
            self.assertGreaterEqual(len(data) - offset, 12)
            length, = struct.unpack(">I", data[offset : offset + 4])
            kind = data[offset + 4 : offset + 8]
            end = offset + 12 + length
            self.assertLessEqual(end, len(data))
            payload = data[offset + 8 : offset + 8 + length]
            checksum, = struct.unpack(">I", data[offset + 8 + length : end])
            self.assertEqual(binascii.crc32(kind + payload) & 0xFFFFFFFF, checksum)
            chunks.append((kind, payload))
            offset = end

        self.assertEqual(chunks[0][0], b"IHDR")
        self.assertEqual(chunks[-1][0], b"IEND")
        self.assertEqual(chunks[-1][1], b"")
        width, height, bit_depth, color_type, compression, filtering, interlace = struct.unpack(">IIBBBBB", chunks[0][1])
        self.assertEqual((width, height, bit_depth, compression, filtering, interlace), (1, 1, 8, 0, 0, 0))
        channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color_type]
        pixels = zlib.decompress(b"".join(payload for kind, payload in chunks if kind == b"IDAT"))
        self.assertEqual(len(pixels), height * (1 + width * channels))
        self.assertLessEqual(pixels[0], 4)


if __name__ == "__main__":
    unittest.main()
