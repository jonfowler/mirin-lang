module add_n #(parameter int n) (
    input  logic clk,
    input  logic [n-1:0] a,
    input  logic [n-1:0] b,
    output logic [n-1:0] result
);
    assign result = (a + b);
endmodule
