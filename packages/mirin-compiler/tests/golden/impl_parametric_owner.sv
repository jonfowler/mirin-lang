module top (
    input  logic clk,
    input  logic b__valid,
    input  logic [7:0] b__data,
    input  logic c__valid,
    input  logic c__data,
    output logic result
);
    logic x;
    Bus__first__uint8 Bus__first__uint8 (
        .clk(clk),
        .self__valid(b__valid),
        .self__data(b__data),
        .result(x)
    );
    logic y;
    Bus__first__bool Bus__first__bool (
        .clk(clk),
        .self__valid(c__valid),
        .self__data(c__data),
        .result(y)
    );
    assign result = x;
endmodule

module Bus__first__bool (
    input  logic clk,
    input  logic self__valid,
    input  logic self__data,
    output logic result
);
    assign result = self__valid;
endmodule

module Bus__first__uint8 (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic result
);
    assign result = self__valid;
endmodule
