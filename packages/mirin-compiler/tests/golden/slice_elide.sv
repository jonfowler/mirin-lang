module lo_half (
    input  logic [15:0] x,
    output logic [7:0] result
);
    assign result = x[7:0];
endmodule

module hi_part (
    input  logic [15:0] x,
    output logic [11:0] result
);
    assign result = x[15:4];
endmodule
