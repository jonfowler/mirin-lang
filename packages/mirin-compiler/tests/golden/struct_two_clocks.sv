module sum_first (
    input  logic c1,
    input  logic c2,
    input  logic [7:0] p__a,
    input  logic [7:0] p__b,
    input  logic [7:0] q__a,
    input  logic [7:0] q__b,
    output logic [7:0] result
);
    assign result = (p__a + p__b);
endmodule
