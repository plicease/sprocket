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
HEADERS := $(wildcard src/*.h)

all: build/SPROCKET.EXE

build:
	mkdir -p build

# -0: 8086 instructions only; -ms: small model; -wx -we: all warnings, as
# errors. -ox optimizes for speed and -s drops the stack check on every
# call (the code has no recursion): together they cut the receive path on a
# 4.77 MHz 8088 from about 590 to 330 us per byte (test/bench.c).
DOS_CFLAGS := -q -bt=dos -ms -0 -ox -s -wx -we

build/SPROCKET.EXE: $(CORE) src/dosport.c src/main.c $(HEADERS) | build
	cd build && $(WCL) $(DOS_CFLAGS) -fe=SPROCKET.EXE \
		$(addprefix ../,$(CORE) src/dosport.c src/main.c)

build/BENCH.EXE: $(CORE) src/dosport.c test/bench.c $(HEADERS) | build
	cd build && $(WCL) $(DOS_CFLAGS) -fe=BENCH.EXE \
		$(addprefix ../,$(CORE) src/dosport.c test/bench.c)

bench: build/BENCH.EXE

build/test_frame: test/test_frame.c test/vectors.c src/frame.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_frame.c test/vectors.c src/frame.c

build/test_proto: test/test_proto.c test/vectors.c $(CORE) $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_proto.c test/vectors.c $(CORE)

build/sprocket-client: $(CORE) src/posixport.c src/main.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ $(CORE) src/posixport.c src/main.c

host: build/sprocket-client

test: build/test_frame build/test_proto
	build/test_frame $(VECTORS)
	build/test_proto $(VECTORS)

clean:
	rm -rf build

.PHONY: all bench host test clean
