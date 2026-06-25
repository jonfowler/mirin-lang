module add7 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    assign result = (x + 7);
endmodule

module add7_dom (
    input  logic clk,
    input  logic [7:0] x,
    output logic [7:0] result
);
    add7 add7 (
        .x(x),
        .result(result)
    );
endmodule
