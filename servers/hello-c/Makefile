# Makefile for hello-c — freestanding C demo for Rost.
#
# Toolchain requirements (macOS):
#   CC       — clang (Apple Clang ≥ 14 or LLVM clang).  Must support
#              --target=x86_64-unknown-none-elf.
#   LLD      — rust-lld (ships with every `rustup` toolchain) or system lld.
#              Used because macOS's ld64 cannot produce ELF64 output.
#
# Output: hello-c   (ELF64 static executable, entry at 0x400000)

CC    = clang
# rust-lld ships with every rustup toolchain and is always on PATH when
# rustup's shims are active.  Fall back to lld if rust-lld is not found.
LLD   = $(shell command -v rust-lld 2>/dev/null || command -v lld 2>/dev/null || echo "rust-lld")

CFLAGS = \
    --target=x86_64-unknown-none-elf \
    -ffreestanding \
    -nostdlib \
    -O2 \
    -fno-pie \
    -fno-stack-protector \
    -fno-asynchronous-unwind-tables \
    -Wall \
    -Wextra

LDFLAGS = \
    -flavor ld \
    -m elf_x86_64 \
    --entry _start \
    -nostdlib \
    -static \
    -T $(CURDIR)/link.ld

TARGET = hello-c

.PHONY: all clean

all: $(TARGET)

$(TARGET): hello.o link.ld
	$(LLD) $(LDFLAGS) -o $@ $<

hello.o: hello.c
	$(CC) $(CFLAGS) -c -o $@ $<

clean:
	rm -f hello.o $(TARGET)
