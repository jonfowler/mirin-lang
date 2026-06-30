module s #(parameter int w) (
    input  logic [15:0] x,
    input  logic [3:0] i,
    output logic [w-1:0] result
);
    logic [w-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> i);
    assign result = __block_0;
endmodule
