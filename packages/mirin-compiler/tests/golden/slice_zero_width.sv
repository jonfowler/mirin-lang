module empty (
    input  logic [7:0] x,
    output logic [0-1:0] result
);
    logic [0-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> 4);
    assign result = __block_0;
endmodule
