module pair_add #(parameter int n, parameter int m) (
    input  logic clk,
    input  logic [n-1:0] a,
    input  logic [m-1:0] b,
    output logic [n-1:0] result
);
    assign result = (a + b);
    initial begin
        assert ((m == n));
    end
endmodule
