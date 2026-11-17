MPICC ?= mpicc
MPIRUN ?= mpirun
MPIRUN_FLAGS ?=
BINARY := ../../target/miniapps/allreduce

.PHONY: all check

all: $(BINARY)

$(BINARY): allreduce.c Makefile
	mkdir -p "$(@D)"
	$(MPICC) -std=c11 -O3 -Wall -Wextra -Werror allreduce.c -o "$@" -lm

check: $(BINARY)
	$(MPIRUN) $(MPIRUN_FLAGS) -n 1 $(BINARY) --count 1 --iterations 2
	$(MPIRUN) $(MPIRUN_FLAGS) -n 2 $(BINARY) --count 257 --iterations 3
	@if $(MPIRUN) $(MPIRUN_FLAGS) -n 2 $(BINARY) --count 0; then echo "zero count was accepted" >&2; exit 1; fi
	@if $(MPIRUN) $(MPIRUN_FLAGS) -n 2 $(BINARY) --count 2147483648; then echo "oversized count was accepted" >&2; exit 1; fi
	@if $(MPIRUN) $(MPIRUN_FLAGS) -n 2 $(BINARY) --iterations -1; then echo "negative iterations were accepted" >&2; exit 1; fi
	@if $(MPIRUN) $(MPIRUN_FLAGS) -n 2 $(BINARY) --count; then echo "missing count was accepted" >&2; exit 1; fi
	@if $(MPIRUN) $(MPIRUN_FLAGS) -n 2 $(BINARY) --unknown; then echo "unknown option was accepted" >&2; exit 1; fi
