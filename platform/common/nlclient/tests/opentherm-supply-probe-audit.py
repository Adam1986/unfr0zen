#!/usr/bin/env python3
"""Offline, exact-build read-only probe audit. Never installs or runs nlclient."""
import hashlib
import struct
import subprocess
import tempfile
from pathlib import Path
import sys

EXPECTED = '5dbb251b03b1c5f63b1450b680ff6b6ba88eaa5e618ddbec8cf1fdc07bbef567'
SLOT = 0x6b7270
ENTRY = 0x846564
OLD = 0x840550
PH = 52 + 8 * 32
HEADER = (1, 0x6f0000, 0x840000, 0x840000, 0x6564, 0x6564, 5, 0x10000)
BINDINGS = '''ENTRY(supply_probe_commit)
SECTIONS { . = 0x846564; .text : { *(.text) } /DISCARD/ : { *(.ARM.attributes) *(.note.GNU-stack) } }
native_get_heatlink_heat = 0xa73ac;
native_get_supply = 0xa73e8;
native_getpid = 0x75ee0;
native_fopen = 0x77918;
native_fwrite = 0x7390c;
native_fclose = 0x74020;
native_rename = 0x76ab4;
existing_device_commit = 0x840550;
'''

def require(condition, message):
    if not condition:
        raise ValueError(message)

def sha(data):
    return hashlib.sha256(data).hexdigest()

def patch(source, payload):
    require(sha(source) == EXPECTED, 'unsupported nlclient SHA256; no output')
    require(len(source) == 0x6f6564, 'wrong source length')
    require(struct.unpack_from('<8I', source, PH) == HEADER, 'unexpected executable segment')
    require(struct.unpack_from('<I', source, SLOT)[0] == OLD, 'unexpected device commit slot')
    require(0 < len(payload) <= 0x10000 - HEADER[4] and len(payload) % 4 == 0, 'payload exceeds reviewed segment')
    out = bytearray(source)
    struct.pack_into('<I', out, SLOT, ENTRY)
    struct.pack_into('<II', out, PH + 16, HEADER[4] + len(payload), HEADER[5] + len(payload))
    out.extend(payload)
    # No native code, original extension or other vtable entries modified.
    edits = set(range(SLOT, SLOT+4)) | set(range(PH+16, PH+24))
    require(all(a == b or i in edits for i, (a, b) in enumerate(zip(source, out))), 'unexpected byte edit')
    # Verify inverse edits and truncation restore exactly the reviewed source.
    restored = bytearray(out[:len(source)])
    for at in (SLOT, PH+16, PH+20):
        restored[at:at+4] = source[at:at+4]
    require(bytes(restored) == source and sha(restored) == EXPECTED, 'rollback mismatch')
    return bytes(out)

def link(assembly):
    with tempfile.TemporaryDirectory() as td:
        d = Path(td)
        (d/'link.ld').write_text(BINDINGS)
        subprocess.run(['clang', '--target=armv7-linux-gnueabihf', '-marm', '-c', str(assembly), '-o', str(d/'probe.o')], check=True)
        subprocess.run(['ld.lld', '-m', 'armelf_linux_eabi', '-static', '-T', str(d/'link.ld'), str(d/'probe.o'), '-o', str(d/'probe.elf')], check=True)
        b = (d/'probe.elf').read_bytes()
        require(b[:7] == b'\x7fELF\x01\x01\x01' and struct.unpack_from('<H', b, 18)[0] == 40, 'expected ARM ELF')
        require(struct.unpack_from('<I', b, 24)[0] == ENTRY, 'wrong entry')
        shoff = struct.unpack_from('<I', b, 32)[0]
        size, count = struct.unpack_from('<HH', b, 46)
        require(size == 40, 'unexpected section format')
        sections = [struct.unpack_from('<10I', b, shoff+i*size) for i in range(count)]
        require(not any(s[1] in (4,9) and s[5] for s in sections), 'unresolved relocations')
        allocated = [s for s in sections if s[2]&2 and s[5]]
        require(len(allocated) == 1, 'unexpected allocated sections')
        s = allocated[0]
        require(s[1] == 1 and s[2] == 6 and s[3] == ENTRY and s[4]+s[5] <= len(b), 'unexpected payload section')
        return b[s[4]:s[4]+s[5]]

def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: opentherm-supply-probe-audit.py /path/to/reviewed-nlclient')
    source = Path(sys.argv[1]).read_bytes()
    require(sha(source) == EXPECTED, 'unsupported nlclient SHA256; no output')
    assembly = Path(__file__).resolve().with_name('opentherm-supply-probe.S')
    payload = link(assembly)
    out = patch(source, payload)
    try:
        patch(source[:-1] + bytes([source[-1]^1]), payload)
    except ValueError:
        pass
    else:
        raise ValueError('modified input accepted')
    print('PASS: exact input hash, native commit slot, RX segment, relocation-free ARM payload, allowed edits, rollback, modified-input rejection')
    print(f'Payload: {len(payload)} bytes; source SHA256: {sha(source)}; candidate SHA256: {sha(out)}')
    print('Audit only: no installable binary written')

if __name__ == '__main__':
    main()
