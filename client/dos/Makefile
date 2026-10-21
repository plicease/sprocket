# SPDX-License-Identifier: MIT
#
# make          build build/SPROCKET.EXE with Open Watcom (8086, small model)
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

# -0: 8086 instructions only; -ms: small model; -os: optimize for size;
# -wx -we: all warnings, as errors.
build/SPROCKET.EXE: $(CORE) src/dosport.c src/main.c $(HEADERS) | build
	cd build && $(WCL) -q -bt=dos -ms -0 -os -wx -we -fe=SPROCKET.EXE \
		$(addprefix ../,$(CORE) src/dosport.c src/main.c)

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

.PHONY: all host test clean
