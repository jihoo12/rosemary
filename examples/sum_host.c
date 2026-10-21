#include "sum.c"
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>

static unsigned char memory[512];

static uint64_t load(void *ctx, uint64_t address, unsigned width) {
    (void)ctx;
    assert(width <= sizeof(memory) && address <= sizeof(memory) - width);
    uint64_t value = 0;
    for (unsigned i = 0; i < width; ++i)
        value |= (uint64_t)memory[address + i] << (8 * i);
    return value;
}

static void store(void *ctx, uint64_t address, unsigned width, uint64_t value) {
    (void)ctx;
    assert(width <= sizeof(memory) && address <= sizeof(memory) - width);
    for (unsigned i = 0; i < width; ++i)
        memory[address + i] = (unsigned char)(value >> (8 * i));
}

int main(void) {
    rosemary_machine machine = {0};
    machine.load = load;
    machine.store = store;
    machine.rip = ROSEMARY_ENTRY;
    machine.r[4] = 256;
    store(NULL, 256, 8, UINT64_C(0xdead));
    int status = rosemary_run(&machine, 100);
    assert(status == 0 && machine.rip == UINT64_C(0xdead));
    assert(machine.r[0] == 15);
    printf("rax=%" PRIu64 "\n", machine.r[0]);
    return 0;
}
