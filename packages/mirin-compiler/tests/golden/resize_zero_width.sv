module to_empty (
    input  logic [7:0] x,
    output logic [0-1:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = (8 != 0) ? type(__block_0)'(x) : '0;
    assign result = __block_0;
endmodule

module from_empty (
    input  logic [7:0] x,
    output logic [3:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> 4);
    logic [3:0] __block_1;
    assign __block_1 = (0 != 0) ? type(__block_1)'(__block_0) : '0;
    assign result = __block_1;
endmodule

module drop_all (
    input  logic [7:0] x,
    output logic [0-1:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> (8 - 0));
    assign result = __block_0;
endmodule

module grow_from_empty (
    input  logic [7:0] x,
    output logic [3:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> 4);
    logic [3:0] __block_1;
    assign __block_1 = (type(__block_1)'(__block_0)) << (4 - 0);
    assign result = __block_1;
endmodule
