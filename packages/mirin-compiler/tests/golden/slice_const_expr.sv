module pick (
    input  logic [15:0] v,
    output logic [3:0] result
);
    logic [3:0] __block_0;
    assign __block_0 = type(__block_0)'(v >> 4);
    assign result = __block_0;
endmodule
