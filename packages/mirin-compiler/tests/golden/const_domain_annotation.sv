module add_k (
    input  logic clk,
    input  logic [7:0] x,
    input  logic [7:0] k,
    output logic [7:0] result
);
    assign result = (x + k);
endmodule
