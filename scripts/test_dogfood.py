"""Regression checks for the optional GNU objdump validation oracle."""
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from dogfood import reference_mnemonic


class ReferenceMnemonicTests(unittest.TestCase):
    def test_prefix_wrappers_can_follow_semantic_prefixes(self):
        for encoded, text, expected in [
            ("f3 41 a4", "rep rex.B movs BYTE PTR es:[rdi],BYTE PTR ds:[rsi]", "rep.movs"),
            ("f3 48 aa", "rep rex.W stos BYTE PTR es:[rdi],al", "rep.stos"),
            ("f3 66 a5", "rep data16 movs WORD PTR es:[rdi],WORD PTR ds:[rsi]", "rep.movs"),
            ("f0 64 48 0f b1 0b", "lock fs cmpxchg QWORD PTR [rbx],rcx", "lock.cmpxchg"),
            ("3e 64 ff 20", "notrack fs jmp QWORD PTR [rax]", "jmp"),
            ("3e 2e ff e0", "ds notrack jmp rax", "notrack.jmp"),
            ("f3 41 90", "rex.B pause", "pause"),
        ]:
            with self.subTest(encoded=encoded, text=text):
                self.assertEqual(reference_mnemonic(encoded.split(), text.split()), expected)

    def test_string_spelling_and_scalar_movsd_are_distinguished_by_opcode(self):
        for encoded, text, expected in [
            ("a4", "movsb", "movs"), ("66 a5", "movsw", "movs"),
            ("a5", "movsd", "movs"), ("48 a5", "movsq", "movs"),
            ("aa", "stosb", "stos"), ("66 ab", "stosw", "stos"),
            ("ab", "stosd", "stos"), ("48 ab", "stosq", "stos"),
            ("f2 0f 10 00", "movsd xmm0,QWORD PTR [rax]", "movsd"),
            ("f2 0f 10 80 00 00 00 a5", "movsd xmm0,QWORD PTR [rax-0x5b000000]", "movsd"),
            ("a4", "unexpected", "unexpected"),
        ]:
            with self.subTest(encoded=encoded):
                self.assertEqual(reference_mnemonic(encoded.split(), text.split()), expected)

    def test_existing_aliases_and_missing_mnemonics(self):
        for encoded, text, expected in [
            ("66 90", "xchg ax,ax", "nop"),
            ("87 c0", "xchg eax,eax", "xchg"),
            ("41 90", "xchg r8d,eax", "xchg"),
            ("48 98", "cdqe", "movsxd"), ("98", "cwde", "movsx"),
            ("66 98", "cbw", "movsx"),
        ]:
            with self.subTest(encoded=encoded):
                self.assertEqual(reference_mnemonic(encoded.split(), text.split()), expected)
        with self.assertRaisesRegex(ValueError, "no mnemonic"):
            reference_mnemonic(["f3", "41"], ["rep", "rex.B"])

    @unittest.skipUnless(shutil.which("objdump"), "optional GNU objdump is unavailable")
    def test_real_gnu_output_for_rex_pause_strings_and_notrack(self):
        cases = [(bytes([0xf3, rex, 0x90]), "pause") for rex in range(0x40, 0x50)]
        for prefix in (b"", b"\x66", b"\x48", b"\x41", b"\x49", b"\x4f"):
            for opcode, name in ((0xa4, "movs"), (0xa5, "movs"), (0xaa, "stos"), (0xab, "stos")):
                cases.append((b"\xf3" + prefix + bytes([opcode]), "rep." + name))
        cases.extend((bytes.fromhex(code), op) for code, op in [
            ("3e ff e0", "notrack.jmp"), ("64 3e ff 20", "jmp"),
            ("3e 65 ff 10", "call"), ("3e 2e ff e0", "notrack.jmp"),
        ])
        expected, code = {}, bytearray()
        for encoded, op in cases:
            expected[len(code)] = (len(encoded), op)
            code.extend(encoded)
        with tempfile.TemporaryDirectory(prefix="rosemary-oracle-") as directory:
            binary = Path(directory) / "cases.bin"
            binary.write_bytes(code)
            output = subprocess.check_output([
                "objdump", "-D", "-b", "binary", "-m", "i386:x86-64", "-M", "intel",
                "--insn-width=16", str(binary),
            ], text=True)
        actual = {}
        for line in output.splitlines():
            match = re.match(r"\s*([0-9a-f]+):\s+((?:[0-9a-f]{2}(?:\s+|$))+)", line)
            if match:
                encoded = match[2].split()
                actual[int(match[1], 16)] = (len(encoded), reference_mnemonic(encoded, line[match.end():].split()))
        self.assertEqual(actual, expected)


if __name__ == "__main__":
    unittest.main()
