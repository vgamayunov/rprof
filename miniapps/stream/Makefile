CC ?= cc
OMPFLAGS ?= -fopenmp
BINARY := ../../target/miniapps/stream

.PHONY: all check

all: $(BINARY)

$(BINARY): stream.c Makefile
	mkdir -p "$(@D)"
	$(CC) -std=c11 -O3 -Wall -Wextra -Werror $(OMPFLAGS) stream.c -o "$@"

check: $(BINARY)
	OMP_NUM_THREADS=1 $(BINARY) --elements 1 --iterations 2
	OMP_NUM_THREADS=2 $(BINARY) --elements 257 --iterations 3 --threads 2
	@if $(BINARY) --elements 0; then echo "zero elements were accepted" >&2; exit 1; fi
	@if $(BINARY) --iterations -1; then echo "negative iterations were accepted" >&2; exit 1; fi
	@if $(BINARY) --threads 0; then echo "zero threads were accepted" >&2; exit 1; fi
	@if $(BINARY) --unknown; then echo "unknown option was accepted" >&2; exit 1; fi
