module hi_zero (
    input  logic [7:0] x,
    input  logic [3:0] y,
    output logic [3:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> 2);
    logic [3:0] __block_1;
    assign __block_1 = (type(__block_1)'(__block_0) << 4)
            | (type(__block_1)'(y) & ~(type(__block_1)'('1) << 4));
    assign result = __block_1;
endmodule

module lo_zero (
    input  logic [7:0] x,
    input  logic [3:0] y,
    output logic [3:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> 2);
    logic [3:0] __block_1;
    assign __block_1 = (type(__block_1)'(y) << 0)
            | (type(__block_1)'(__block_0) & ~(type(__block_1)'('1) << 0));
    assign result = __block_1;
endmodule
