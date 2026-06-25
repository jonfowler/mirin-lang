module add (
    input  logic [7:0] p__a,
    input  logic [7:0] p__b,
    output logic [7:0] result
);
    logic [7:0] x;
    assign x = p__a;
    logic [7:0] y;
    assign y = p__b;
    assign result = (x + y);
endmodule

module corner (
    input  logic n__tag,
    input  logic [7:0] n__inner__a,
    input  logic [7:0] n__inner__b,
    input  logic [7:0] t__0__a,
    input  logic [7:0] t__0__b,
    input  logic t__1,
    output logic [7:0] result
);
    logic _flag;
    assign _flag = n__tag;
    logic [7:0] __pat5__a;
    logic [7:0] __pat5__b;
    assign __pat5__a = n__inner__a;
    assign __pat5__b = n__inner__b;
    logic [7:0] lo;
    assign lo = __pat5__a;
    logic [7:0] hi;
    assign hi = __pat5__b;
    logic [7:0] __pat8__a;
    logic [7:0] __pat8__b;
    assign __pat8__a = t__0__a;
    assign __pat8__b = t__0__b;
    logic [7:0] u;
    assign u = __pat8__a;
    logic [7:0] v;
    assign v = __pat8__b;
    logic _take;
    assign _take = t__1;
    assign result = (((lo + hi) + u) + v);
endmodule
