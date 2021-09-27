#!/bin/sh

GREP=$(which rg) || $(which grep)
MAKE=${MAKE:-$(which make)}
PATH="./zig-out/bin:${PATH}"

echo ".POSIX:\n.DEFAULT: all\n" > Makefile

git -C ~/src/Nim rev-list --objects --all --filter=object:type=blob --pretty=format: | \
	${GREP} -v '^commit ' | \
	sort | \
	uniq | \
	gawsh-gen-make >> Makefile

echo 'all: \' >> Makefile
${GREP} -v '^\s' Makefile | ${GREP} -v '^(all|\.POSIX|\.DEFAULT):' | sed 's/:$//' | awk '{ print "\t" $0 " \\" }'  >> Makefile

${MAKE} -j$(nproc) all
