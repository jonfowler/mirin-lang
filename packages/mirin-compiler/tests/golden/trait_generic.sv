module uint__Amp__amp #(parameter int n) (
    input  logic [n-1:0] self,
    output logic [n-1:0] result
);
    assign result = (self + self);
endmodule

module top (
    input  logic clk,
    input  logic [7:0] x,
    output logic [7:0] result
);
    boost__uint8 boost__uint8 (
        .x(x),
        .result(result)
    );
endmodule

module boost__uint8 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    uint__Amp__amp #(
        .n(8)
    ) uint__Amp__amp (
        .self(x),
        .result(result)
    );
endmodule
