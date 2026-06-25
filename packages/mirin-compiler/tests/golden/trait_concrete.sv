module Sample__Scale__scaled (
    input  logic [7:0] self__value,
    input  logic [7:0] k,
    output logic [7:0] result
);
    assign result = (self__value * k);
endmodule

module top (
    input  logic clk,
    input  logic [7:0] s__value,
    input  logic [7:0] k,
    output logic [7:0] result
);
    Sample__Scale__scaled Sample__Scale__scaled (
        .self__value(s__value),
        .k(k),
        .result(result)
    );
endmodule

module bool__Invert__invert (
    input  logic self,
    output logic result
);
    assign result = self;
endmodule

module top2 (
    input  logic clk,
    input  logic x,
    output logic result
);
    bool__Invert__invert bool__Invert__invert (
        .self(x),
        .result(result)
    );
endmodule
