module pack2 (
    input  logic [7:0] a,
    input  logic [3:0] b,
    output logic [11:0] result
);
    Tuple__BitPack__pack__uint8__uint4 Tuple__BitPack__pack__uint8__uint4 (
        .self__0(a),
        .self__1(b),
        .result(result)
    );
endmodule

module unpack2 (
    input  logic [11:0] w,
    output logic [7:0] result__0,
    output logic [3:0] result__1
);
    Tuple__BitPack__unpack__uint8__uint4 Tuple__BitPack__unpack__uint8__uint4 (
        .b(w),
        .result__0(result__0),
        .result__1(result__1)
    );
endmodule

module pack3 (
    input  logic [7:0] a,
    input  logic [3:0] b,
    input  logic c,
    output logic [12:0] result
);
    Tuple__BitPack__pack__uint8__uint4__bool Tuple__BitPack__pack__uint8__uint4__bool (
        .self__0(a),
        .self__1(b),
        .self__2(c),
        .result(result)
    );
endmodule

module unpack3 (
    input  logic [12:0] w,
    output logic [7:0] result__0,
    output logic [3:0] result__1,
    output logic result__2
);
    Tuple__BitPack__unpack__uint8__uint4__bool Tuple__BitPack__unpack__uint8__uint4__bool (
        .b(w),
        .result__0(result__0),
        .result__1(result__1),
        .result__2(result__2)
    );
endmodule

module roundtrip3 (
    input  logic [7:0] a,
    input  logic [3:0] b,
    input  logic c,
    output logic [7:0] result__0,
    output logic [3:0] result__1,
    output logic result__2
);
    logic [12:0] w;
    Tuple__BitPack__pack__uint8__uint4__bool Tuple__BitPack__pack__uint8__uint4__bool (
        .self__0(a),
        .self__1(b),
        .self__2(c),
        .result(w)
    );
    Tuple__BitPack__unpack__uint8__uint4__bool Tuple__BitPack__unpack__uint8__uint4__bool (
        .b(w),
        .result__0(result__0),
        .result__1(result__1),
        .result__2(result__2)
    );
endmodule

module Tuple__BitPack__pack__uint8__uint4 (
    input  logic [7:0] self__0,
    input  logic [3:0] self__1,
    output logic [11:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = self__0;
    logic [3:0] __block_1;
    assign __block_1 = self__1;
    logic [11:0] __block_2;
    assign __block_2 = (type(__block_2)'(__block_1) << 8)
            | (type(__block_2)'(__block_0) & ~(type(__block_2)'('1) << 8));
    assign result = __block_2;
endmodule

module Tuple__BitPack__pack__uint8__uint4__bool (
    input  logic [7:0] self__0,
    input  logic [3:0] self__1,
    input  logic self__2,
    output logic [12:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = self__0;
    logic [3:0] __block_1;
    assign __block_1 = self__1;
    logic [11:0] __block_2;
    assign __block_2 = (type(__block_2)'(__block_1) << 8)
            | (type(__block_2)'(__block_0) & ~(type(__block_2)'('1) << 8));
    logic [0:0] __block_3;
    assign __block_3 = self__2;
    logic [12:0] __block_4;
    assign __block_4 = (type(__block_4)'(__block_3) << 12)
            | (type(__block_4)'(__block_2) & ~(type(__block_4)'('1) << 12));
    assign result = __block_4;
endmodule

module Tuple__BitPack__unpack__uint8__uint4 (
    input  logic [11:0] b,
    output logic [7:0] result__0,
    output logic [3:0] result__1
);
    logic [7:0] ea;
    logic [7:0] __block_0;
    assign __block_0 = type(__block_0)'(b);
    assign ea = __block_0;
    logic [3:0] eb;
    logic [3:0] __block_1;
    assign __block_1 = type(__block_1)'(b >> (12 - 4));
    assign eb = __block_1;
    logic [7:0] __block_2;
    assign __block_2 = ea;
    logic [3:0] __block_3;
    assign __block_3 = eb;
    assign result__0 = __block_2;
    assign result__1 = __block_3;
endmodule

module Tuple__BitPack__unpack__uint8__uint4__bool (
    input  logic [12:0] b,
    output logic [7:0] result__0,
    output logic [3:0] result__1,
    output logic result__2
);
    logic [7:0] ea;
    logic [7:0] __block_0;
    assign __block_0 = type(__block_0)'(b);
    assign ea = __block_0;
    logic [4:0] r1;
    logic [4:0] __block_1;
    assign __block_1 = type(__block_1)'(b >> (13 - 5));
    assign r1 = __block_1;
    logic [3:0] eb;
    logic [3:0] __block_2;
    assign __block_2 = type(__block_2)'(r1);
    assign eb = __block_2;
    logic [0:0] ec;
    logic [0:0] __block_3;
    assign __block_3 = type(__block_3)'(r1 >> (5 - 1));
    assign ec = __block_3;
    logic [7:0] __block_4;
    assign __block_4 = ea;
    logic [3:0] __block_5;
    assign __block_5 = eb;
    logic __block_6;
    assign __block_6 = ec;
    assign result__0 = __block_4;
    assign result__1 = __block_5;
    assign result__2 = __block_6;
endmodule
