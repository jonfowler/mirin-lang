module top (
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] p__a;
    logic [7:0] p__b;
    assign p__a = x;
    assign p__b = x;
    assign result = (p__a + p__b);
endmodule
