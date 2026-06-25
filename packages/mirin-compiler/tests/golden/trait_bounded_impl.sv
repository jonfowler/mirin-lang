module bool__Big__big (
    input  logic self,
    output logic result
);
    assign result = self;
endmodule

module top (
    input  logic clk,
    input  logic p__a__a,
    input  logic p__a__b,
    input  logic p__b__a,
    input  logic p__b__b,
    output logic result
);
    Pair__Big__big__Pair Pair__Big__big__Pair (
        .self__a__a(p__a__a),
        .self__a__b(p__a__b),
        .self__b__a(p__b__a),
        .self__b__b(p__b__b),
        .result(result)
    );
endmodule

module Pair__Big__big__Pair (
    input  logic self__a__a,
    input  logic self__a__b,
    input  logic self__b__a,
    input  logic self__b__b,
    output logic result
);
    Pair__Big__big__bool Pair__Big__big__bool (
        .self__a(self__a__a),
        .self__b(self__a__b),
        .result(result)
    );
endmodule

module Pair__Big__big__bool (
    input  logic self__a,
    input  logic self__b,
    output logic result
);
    bool__Big__big bool__Big__big (
        .self(self__a),
        .result(result)
    );
endmodule
