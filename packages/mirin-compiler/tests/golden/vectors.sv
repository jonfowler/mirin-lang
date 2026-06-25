module f (
    input  logic clk,
    input  logic [3:0] a,
    input  logic [3:0] b,
    input  logic [1:0] sel,
    input  logic [7:0] raw,
    output logic [3:0] result
);
    logic [3:0] v [0:2];
    assign v = '{a, b, 5};
    logic [3:0] z [0:2];
    assign z = '{3{0}};
    logic pixels__on [0:1];
    logic [3:0] pixels__val [0:1];
    assign pixels__on = '{1'b1, raw[0]};
    assign pixels__val = '{a, b};
    logic [3:0] first;
    assign first = v[0];
    logic [3:0] dyn_pick;
    always_comb assert (sel < 3);
    assign dyn_pick = v[sel];
    logic [3:0] zz;
    assign zz = z[1];
    assign result = ((first + dyn_pick) + zz);
endmodule
