module widen (
    input  logic clk,
    input  logic [7:0] a,
    output logic [15:0] result
);
    assign result = ({{(16 - 8){1'b0}}, a});
endmodule

module narrow (
    input  logic clk,
    input  logic [7:0] a,
    output logic [3:0] result
);
    assign result = (a[4 - 1 : 0]);
endmodule

module sign_widen (
    input  logic clk,
    input  logic signed [7:0] a,
    input  logic signed [15:0] b,
    output logic signed [15:0] result
);
    assign result = (($signed({{(16 - 8){a[8 - 1]}}, a})) + b);
endmodule

module scale_up (
    input  logic clk,
    input  logic [7:0] a,
    output logic [11:0] result
);
    assign result = ({a, {(12 - 8){1'b0}}});
endmodule

module scale_down (
    input  logic clk,
    input  logic [7:0] a,
    output logic [3:0] result
);
    assign result = (a[8 - 1 : 8 - 4]);
endmodule

module resize_up (
    input  logic clk,
    input  logic [7:0] a,
    output logic [11:0] result
);
    assign result = (12'(a));
endmodule

module resize_down (
    input  logic clk,
    input  logic signed [7:0] a,
    output logic signed [3:0] result
);
    assign result = (4'(a));
endmodule
