#!/usr/bin/env python3

from zipfile import ZipFile
import os
import random

DEST = "assets"
SMALL = 8
LARGE = 10 * 1024 * 1024

def make_files(dest: str):
    with open(os.path.join(dest, "small"), "wb") as f:
        f.write(bytes([random.randrange(256) for _ in range(SMALL)]))
    with open(os.path.join(dest, "large"), "wb") as f:
        f.write(bytes([random.randrange(256) for _ in range(LARGE)]))

def make_archive(dest: str):
    with ZipFile(os.path.join(dest, "test.zip"), mode="w") as z:
        z.write(os.path.join(dest, "small"), "small")
        z.write(os.path.join(dest, "large"), "large")

def main():
    os.makedirs(DEST, exist_ok=True)
    make_files(DEST)
    make_archive(DEST)

if __name__ == "__main__":
    main()
