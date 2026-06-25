module passthrough (
    input  logic clk,
    input  logic [7:0] v [0:2],
    output logic [7:0] result [0:2]
);
    assign result = v;
endmodule
