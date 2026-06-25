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
    assign result = ({(self__1), (self__0)});
endmodule

module Tuple__BitPack__pack__uint8__uint4__bool (
    input  logic [7:0] self__0,
    input  logic [3:0] self__1,
    input  logic self__2,
    output logic [12:0] result
);
    assign result = ({(self__2), ({(self__1), (self__0)})});
endmodule

module Tuple__BitPack__unpack__uint8__uint4 (
    input  logic [11:0] b,
    output logic [7:0] result__0,
    output logic [3:0] result__1
);
    logic [7:0] ea;
    assign ea = (b[8 - 1 : 0]);
    logic [3:0] eb;
    assign eb = (b[12 - 1 : 12 - 4]);
    assign result__0 = (ea);
    assign result__1 = (eb);
endmodule

module Tuple__BitPack__unpack__uint8__uint4__bool (
    input  logic [12:0] b,
    output logic [7:0] result__0,
    output logic [3:0] result__1,
    output logic result__2
);
    logic [7:0] ea;
    assign ea = (b[8 - 1 : 0]);
    logic [4:0] r1;
    assign r1 = (b[13 - 1 : 13 - 5]);
    logic [3:0] eb;
    assign eb = (r1[4 - 1 : 0]);
    logic [0:0] ec;
    assign ec = (r1[5 - 1 : 5 - 1]);
    assign result__0 = (ea);
    assign result__1 = (eb);
    assign result__2 = (ec);
endmodule
