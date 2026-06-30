module hi_zero (
    input  logic [7:0] x,
    input  logic [3:0] y,
    output logic [3:0] result
);
    logic [3:0] __inl0__self;
    assign __inl0__self = y;
    logic [7:0] __inl1__self;
    assign __inl1__self = x;
    logic [0-1:0] __inl1____block_0;
    assign __inl1____block_0 = '0;
    logic [0-1:0] __inl0__hi;
    assign __inl0__hi = __inl1____block_0;
    logic [3:0] __inl0____block_0;
    assign __inl0____block_0 = 4'(__inl0__self);
    assign result = __inl0____block_0;
endmodule

module lo_zero (
    input  logic [7:0] x,
    input  logic [3:0] y,
    output logic [3:0] result
);
    logic [7:0] __inl1__self;
    assign __inl1__self = x;
    logic [0-1:0] __inl1____block_0;
    assign __inl1____block_0 = '0;
    logic [0-1:0] __inl0__self;
    assign __inl0__self = __inl1____block_0;
    logic [3:0] __inl0__hi;
    assign __inl0__hi = y;
    logic [3:0] __inl0____block_0;
    assign __inl0____block_0 = 4'(__inl0__hi);
    assign result = __inl0____block_0;
endmodule
