CUDA_HOME ?= /usr/local/cuda
MKLROOT ?= /opt/intel/oneapi/mkl/latest
IOMP_LIBDIR ?= /opt/intel/oneapi/compiler/latest/lib
NVCC ?= $(CUDA_HOME)/bin/nvcc
BINARY := ../../target/miniapps/gemm

.PHONY: all check

all: $(BINARY)

$(BINARY): gemm.cu Makefile
	mkdir -p "$(@D)"
	$(NVCC) -std=c++17 -O3 -lineinfo -Xcompiler=-Wall,-Wextra,-Werror \
		-I"$(MKLROOT)/include" gemm.cu -o "$@" \
		-L"$(MKLROOT)/lib" -L"$(IOMP_LIBDIR)" -L"$(CUDA_HOME)/lib64" \
		-Xlinker --no-as-needed \
		-lmkl_intel_lp64 -lmkl_intel_thread -lmkl_core -liomp5 \
		-lcublas -lpthread -lm -ldl \
		-Xlinker -rpath -Xlinker "$(MKLROOT)/lib" \
		-Xlinker -rpath -Xlinker "$(IOMP_LIBDIR)" \
		-Xlinker -rpath -Xlinker "$(CUDA_HOME)/lib64"

check: $(BINARY)
	$(BINARY) --size 17 --iterations 2 --threads 1
	$(BINARY) --size 257 --iterations 2 --threads 2
	@if $(BINARY) --size 0; then echo "zero size was accepted" >&2; exit 1; fi
	@if $(BINARY) --iterations -1; then echo "negative iterations were accepted" >&2; exit 1; fi
	@if $(BINARY) --unknown; then echo "unknown option was accepted" >&2; exit 1; fi
