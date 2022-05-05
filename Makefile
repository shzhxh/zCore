# Makefile for top level of zCore

# Possible things to $(MAKE):
#
# rootfs-x64 : contains alpine-rootfs for x86_64, and compiled binary of linux-syscall/test/*.c
# riscv-rootfs : prebuilt binary for riscv64, contains busybox, libc-test and oscomp
# libc-test : build binary of libc-test for x86_64
# rcore-fs-fuse : install a tool called rcore-fs-fuse
# image : make a normal x86_64 image from alpine-rootfs
# riscv-image : make a riscv64 image from riscv-rootfs for testing
# clean : delete all files generated by compilation
# doc : cargo doc --open
# baremetal-test-img : make a x86_64 image for testing

ROOTFS_TAR := minirootfs.tar.gz
LIBC_TEST_URL := https://github.com/rcore-os/libc-test.git

PATH := $(PATH):$(PWD)/toolchain/riscv64-linux-musl-cross/bin

ARCH ?= x86_64
OUT_IMG := zCore/$(ARCH).img
TMP_ROOTFS := /tmp/rootfs

# for linux syscall tests
TEST_DIR := linux-syscall/test/
DEST_DIR := rootfs/bin/
TEST_PATH := $(wildcard $(TEST_DIR)*.c)
BASENAMES := $(notdir $(basename $(TEST_PATH)))

CFLAG := -Wl,--dynamic-linker=/lib/ld-musl-x86_64.so.1

.PHONY: rootfs libc-test rcore-fs-fuse image

rootfs:
	cargo rootfs x86_64
	@for VAR in $(BASENAMES); do gcc $(TEST_DIR)$$VAR.c -o $(DEST_DIR)$$VAR $(CFLAG); done

riscv-rootfs:
	cargo rootfs riscv64

libc-test:
	cd rootfs && git clone $(LIBC_TEST_URL) --depth 1
	cd rootfs/libc-test && cp config.mak.def config.mak && echo 'CC := musl-gcc' >> config.mak && make -j

rt-test:
	cd rootfs && git clone https://kernel.googlesource.com/pub/scm/linux/kernel/git/clrkwllms/rt-tests --depth 1
	cd rootfs/rt-tests && make
	echo x86 gcc build rt-test,now need manual modificy.

rcore-fs-fuse:
	cargo xtask fs-fuse

$(OUT_IMG): rootfs rcore-fs-fuse
	@echo Generating $(ARCH).img
	@rm -rf $(TMP_ROOTFS)
	@mkdir -p $(TMP_ROOTFS)
	@tar xf prebuilt/linux/x86_64/$(ROOTFS_TAR) -C $(TMP_ROOTFS)
	@cp $(TMP_ROOTFS)/lib/ld-musl-x86_64.so.1 rootfs/lib/
	@rcore-fs-fuse $@ rootfs zip
# recover rootfs/ld-musl-x86_64.so.1 for zcore usr libos
# libc-libos.so (convert syscall to function call) is from https://github.com/rcore-os/musl/tree/rcore
	@cp prebuilt/linux/libc-libos.so rootfs/lib/ld-musl-x86_64.so.1

image: $(OUT_IMG)
	@echo Resizing $(OUT_IMG)
	@qemu-img resize $(OUT_IMG) +5M

baremetal-test-img: rootfs rcore-fs-fuse
	@echo Generating $(OUT_IMG)
	@rm -rf $(TMP_ROOTFS)
	@mkdir -p $(TMP_ROOTFS)
	@tar xf prebuilt/linux/x86_64/$(ROOTFS_TAR) -C $(TMP_ROOTFS)
	@mkdir -p rootfs/lib
	@cp $(TMP_ROOTFS)/lib/ld-musl-x86_64.so.1 rootfs/lib/
	@cd rootfs && rm -rf libc-test && git clone $(LIBC_TEST_URL) --depth 1
	@cd rootfs/libc-test && cp config.mak.def config.mak && echo 'CC := musl-gcc' >> config.mak && make -j
	@rcore-fs-fuse $(OUT_IMG) rootfs zip
# recover rootfs/ld-musl-x86_64.so.1 for zcore usr libos
# libc-libos.so (convert syscall to function call) is from https://github.com/rcore-os/musl/tree/rcore
	@cp prebuilt/linux/libc-libos.so rootfs/lib/ld-musl-x86_64.so.1
	@echo Resizing $(OUT_IMG)
	@qemu-img resize $(OUT_IMG) +5M

riscv-image: rcore-fs-fuse riscv-rootfs
	@echo Generating $(OUT_IMG)
	@cd riscv_rootfs && mv libc-test libc-test-prebuild
	@cd riscv_rootfs && git clone $(LIBC_TEST_URL) --depth 1
	@cd riscv_rootfs/libc-test && cp config.mak.def config.mak && make ARCH=riscv64 CROSS_COMPILE=riscv64-linux-musl- -j
	@cd riscv_rootfs && cp libc-test-prebuild/functional/tls_align-static.exe libc-test/src/functional/
	@rcore-fs-fuse zCore/riscv64.img riscv_rootfs zip
	@qemu-img resize -f raw zCore/riscv64.img +5M

check:
	cargo xtask check

doc:
	cargo doc --open

clean:
	cargo clean
	find zCore -maxdepth 1 -name "*.img" -delete
	rm -rf rootfs
	rm -rf riscv_rootfs
	rm -rf toolchain
	find zCore/target -type f -name "*.zbi" -delete
	find zCore/target -type f -name "*.elf" -delete
	cd linux-syscall/test-oscomp && make clean
	cd linux-syscall/busybox && make clean
	cd linux-syscall/lua && make clean
	cd linux-syscall/lmbench && make clean
