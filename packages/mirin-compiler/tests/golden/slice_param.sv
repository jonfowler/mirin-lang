module drop_low #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [(n - 1)-1:0] result
);
    logic [(n - 1)-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x >> 1);
    assign result = __block_0;
endmodule
