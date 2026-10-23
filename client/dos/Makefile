# SPDX-License-Identifier: MIT
#
# make          build build/SPROCKET.EXE with Open Watcom (8086, small model)
# make bench    build build/BENCH.EXE, which times the receive path on DOS
# make test     build and run the host tests against tests/vectors/
# make host     build build/sprocket-client, the client for a Linux tty/PTY
#
# WATCOM is the Open Watcom install directory.

WATCOM ?= $(HOME)/opt/watcom
export WATCOM
export INCLUDE := $(WATCOM)/h
export PATH := $(WATCOM)/binl64:$(PATH)
WCL := wcl

CC ?= cc
HOST_CFLAGS := -std=c99 -Wall -Wextra -pedantic -Werror -O2

VECTORS := ../../tests/vectors
CORE := src/frame.c src/proto.c
TREE := src/tree.c src/crc32.c
HEADERS := $(wildcard src/*.h)

all: build/SPROCKET.EXE

build:
	mkdir -p build

# -0: 8086 instructions only; -ms: small model; -wx -we: all warnings, as
# errors. -ox optimizes for speed and -s drops the stack check on every
# call (the code has no recursion): together they cut BENCH's per-byte figure
# under 86Box (4.77 MHz 8088) from about 590 to 330 us (test/bench.c, in a
# version that also counted the frame check).
DOS_CFLAGS := -q -bt=dos -ms -0 -ox -s -wx -we

DOS_SRC := $(CORE) $(TREE) src/dosfs.c src/dosport.c src/main.c
HOST_SRC := $(CORE) $(TREE) src/posixfs.c src/posixport.c src/main.c

build/SPROCKET.EXE: $(DOS_SRC) $(HEADERS) | build
	cd build && $(WCL) $(DOS_CFLAGS) -fe=SPROCKET.EXE $(addprefix ../,$(DOS_SRC))

build/BENCH.EXE: $(CORE) src/dosport.c test/bench.c $(HEADERS) | build
	cd build && $(WCL) $(DOS_CFLAGS) -fe=BENCH.EXE \
		$(addprefix ../,$(CORE) src/dosport.c test/bench.c)

bench: build/BENCH.EXE

build/test_frame: test/test_frame.c test/vectors.c src/frame.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_frame.c test/vectors.c src/frame.c

build/test_proto: test/test_proto.c test/vectors.c $(CORE) $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_proto.c test/vectors.c $(CORE)

build/test_tree: test/test_tree.c test/vectors.c $(CORE) $(TREE) src/posixfs.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_tree.c test/vectors.c $(CORE) $(TREE) src/posixfs.c

build/sprocket-client: $(HOST_SRC) $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ $(HOST_SRC)

host: build/sprocket-client

test: build/test_frame build/test_proto build/test_tree
	build/test_frame $(VECTORS)
	build/test_proto $(VECTORS)
	build/test_tree $(VECTORS)

clean:
	rm -rf build

.PHONY: all bench host test clean
