module Sample__scaled (
    input  logic clk,
    input  logic [7:0] self__value,
    input  logic [7:0] k,
    output logic [7:0] result
);
    assign result = (self__value * k);
endmodule

module Sample__biased (
    input  logic clk,
    input  logic [7:0] self__value,
    input  logic [7:0] k,
    input  logic [7:0] c,
    output logic [7:0] result
);
    logic [7:0] __call_0;
    Sample__scaled Sample__scaled (
        .clk(clk),
        .self__value(self__value),
        .k(k),
        .result(__call_0)
    );
    assign result = (__call_0 + c);
endmodule

module top (
    input  logic clk,
    input  logic [7:0] s__value,
    input  logic [7:0] k,
    input  logic [7:0] c,
    output logic [7:0] result
);
    Sample__biased Sample__biased (
        .clk(clk),
        .self__value(s__value),
        .k(k),
        .c(c),
        .result(result)
    );
endmodule
