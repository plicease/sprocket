# SPDX-License-Identifier: MIT
#
# make          build build/SPROCKET.EXE with Open Watcom (8086, small model)
# make bench    build build/BENCH.EXE, which times the receive path on DOS
# make test     build and run the host tests against tests/vectors/ and tests/shell/
# make host     build build/sprocket-client, the same program for a Linux tty/PTY
#
# WATCOM is the Open Watcom install directory. MAX_PAYLOAD=n builds with a
# larger payload (up to 4090), for measuring disk imaging speed.

WATCOM ?= $(HOME)/opt/watcom
export WATCOM
export INCLUDE := $(WATCOM)/h
export PATH := $(WATCOM)/binl64:$(PATH)
WCL := wcl

CC ?= cc
HOST_CFLAGS := -std=c99 -Wall -Wextra -pedantic -Werror -O2
ifdef MAX_PAYLOAD
HOST_CFLAGS += -DMAX_PAYLOAD=$(MAX_PAYLOAD)
endif

VECTORS := ../../tests/vectors
SHELL_FIXTURES := ../../tests/shell
CORE := src/frame.c src/proto.c
TREE := src/tree.c src/crc32.c
CLIENT := src/cli.c src/place.c src/req.c src/shell.c
HEADERS := $(wildcard src/*.h)

all: build/SPROCKET.EXE

build:
	mkdir -p build

# -0: 8086 instructions only; -ms: small model; -wx -we: all warnings, as
# errors. -ox optimizes for speed and -s drops the stack check on every
# call: together they cut BENCH's per-byte figure under 86Box (4.77 MHz
# 8088) from about 590 to 330 us (test/bench.c, in a version that also
# counted the frame check). Without the check the stack must be big enough
# by design: the only recursion is ls -R, at most 32 levels of a small
# frame, so -k8192 gives a stack of 8 KB, several times what is used.
DOS_CFLAGS := -q -bt=dos -ms -0 -ox -s -wx -we -k8192
ifdef MAX_PAYLOAD
DOS_CFLAGS += -dMAX_PAYLOAD=$(MAX_PAYLOAD)
endif

DOS_SRC := $(CORE) $(TREE) $(CLIENT) src/storage.c src/biosdisk.c src/dosfs.c src/dosport.c \
	src/main.c
HOST_SRC := $(CORE) $(TREE) $(CLIENT) src/storage.c src/imgdisk.c src/posixfs.c \
	src/posixport.c src/main.c

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

build/test_tree: test/test_tree.c test/vectors.c $(CORE) $(TREE) src/place.c src/posixfs.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_tree.c test/vectors.c $(CORE) $(TREE) src/place.c src/posixfs.c

build/test_shell: test/test_shell.c test/vectors.c src/shell.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_shell.c test/vectors.c src/shell.c

build/test_storage: test/test_storage.c src/storage.c $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ test/test_storage.c src/storage.c

build/sprocket-client: $(HOST_SRC) $(HEADERS) | build
	$(CC) $(HOST_CFLAGS) -o $@ $(HOST_SRC)

host: build/sprocket-client

test: build/test_frame build/test_proto build/test_tree build/test_shell build/test_storage
	build/test_frame $(VECTORS)
	build/test_proto $(VECTORS)
	build/test_tree $(VECTORS)
	build/test_shell $(SHELL_FIXTURES)
	build/test_storage

clean:
	rm -rf build

.PHONY: all bench host test clean
