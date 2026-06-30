module to_empty (
    input  logic [7:0] x,
    output logic [0-1:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x);
    assign result = __block_0;
endmodule

module from_empty (
    input  logic [7:0] x,
    output logic [3:0] result
);
    logic [7:0] __inl0__self;
    assign __inl0__self = x;
    logic [0-1:0] __inl0____block_0;
    assign __inl0____block_0 = '0;
    logic [3:0] __block_1;
    assign __block_1 = type(__block_1)'(__inl0____block_0);
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
    logic [7:0] __inl0__self;
    assign __inl0__self = x;
    logic [0-1:0] __inl0____block_0;
    assign __inl0____block_0 = '0;
    logic [3:0] __block_1;
    assign __block_1 = (type(__block_1)'(__inl0____block_0)) << (4 - 0);
    assign result = __block_1;
endmodule
